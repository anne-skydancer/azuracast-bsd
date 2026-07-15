//! Live-DJ harbor input (Phase 4): a TCP listener that accepts an incoming
//! source-client connection, authenticates it against PHP (`auth`, SPEC.md
//! D.2), decodes its live audio stream incrementally, and gives it playback
//! priority over AutoDJ -- `engine/SPEC.md` B.4 (`writeHarborConfiguration`)
//! and C.4 (DJ authentication / connect / disconnect sequencing).
//!
//! ## Scope
//!
//! - **Protocol**: only the Icecast2/HTTP-style source handshake -- a
//!   `SOURCE <mount> ICE/1.0` or `PUT <mount> HTTP/1.0` request line,
//!   HTTP-style headers terminated by a blank line, credentials via
//!   `Authorization: Basic <base64(user:pass)>`. This is what modern source
//!   clients (BUTT, Mixxx, ffmpeg's icecast muxer) send. Legacy
//!   Shoutcast1/2 framing (non-HTTP-style) is explicitly **not**
//!   implemented.
//! - **Decode**: streaming/chunked via `symphonia`, wrapping the live TCP
//!   connection directly as a `symphonia::core::io::ReadOnlySource` (an
//!   unseekable `MediaSource` -- confirmed via `symphonia-core`'s own
//!   source, `src/io/mod.rs`, which exists precisely for this
//!   non-seekable-stream case). This is a separate code path from
//!   `decode.rs`'s full-buffer AutoDJ-file decode; that path is untouched.
//! - **Not implemented** (deferred, see task/README): mid-stream ICY
//!   metadata updates from the source client, and recording the live
//!   stream to disk (SPEC.md B.4 point 4, `record_streams`).
//! - **Readiness**: a connection becomes the active playback source once
//!   it is connected + authenticated + has produced at least one decoded
//!   chunk of audio -- a deliberate simplification of SPEC.md C.4's literal
//!   `live_connected` timing (which flips `live_enabled` true at raw
//!   TCP-accept time, before any audio has actually decoded), per this
//!   phase's own task framing ("becoming the active source the moment it's
//!   connected+authenticated+producing decoded audio").
//!
//! ## Pipeline integration (see `pipeline.rs`'s module doc for the other
//! half of this)
//!
//! Each decoded chunk of live audio is wrapped as a `PreparedTrack` (via
//! `prepare::prepare_live_chunk`) so it flows through the existing
//! AutoDJ-track-shaped pipeline loop with minimal special-casing. Priority
//! resolution (SPEC.md C.8) is `interrupting_requests` > live > `requests`
//! > AutoDJ; `autodj::fetch_next_track` checks `LiveState::is_ready` in
//! that exact slot. The one-shot "to-live" edge (SPEC.md B.4 #3's
//! `check_live()`) is `LiveState::poll_transition`, polled once per
//! pipeline loop iteration; `pipeline.rs` uses that edge both to force an
//! AutoDJ skip (SPEC.md B.4 #3, reusing the existing `/skip` mechanism from
//! `control.rs`) and to gate the to-live crossfade branch
//! (`crossfade.rs::CrossfadeParams::to_live`).

use std::collections::HashMap;
use std::io::{BufRead, Cursor, Read};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::json;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::{MediaSourceStream, ReadOnlySource};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, Mutex as TokioMutex};

use crate::callbacks::CallbackClient;
use crate::config::HarborConfig;
use crate::decode::{
    append_interleaved, interleave_stereo, to_stereo_channel_planes, DecodedTrack,
    StreamResampler, PIPELINE_CHANNELS, PIPELINE_SAMPLE_RATE,
};
use crate::prepare::{prepare_live_chunk, PreparedTrack};

/// SPEC.md C.1's `live_broadcast_text` setting default. Not station
/// configurable via this engine's config contract yet (same documented-
/// follow-up status as other C.1 settings this engine hasn't threaded
/// through `engine.toml`, e.g. `crossfade`'s pre-Phase-3 defaults).
const DEFAULT_LIVE_BROADCAST_TEXT: &str = "Live Broadcast";

/// Cap on the raw request-line + header block read off a connecting
/// source client, before any handshake parsing happens -- a defensive
/// limit against a slow/malicious client streaming an unbounded "header"
/// that never terminates with a blank line. Generously large for any real
/// source client (a handful of headers, well under 1KB in practice).
const MAX_HEADER_BLOCK_BYTES: usize = 8192;

// ---------------------------------------------------------------------
// Handshake parsing (pure, unit-testable)
// ---------------------------------------------------------------------

/// Parsed Icecast2/HTTP-style source-client handshake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarborHandshake {
    pub mount: String,
    pub user: String,
    pub password: String,
    pub content_type: Option<String>,
}

/// Parses a `SOURCE <mount> ICE/1.0` or `PUT <mount> HTTP/1.0` request line
/// plus HTTP-style headers (terminated by a blank line) from `reader`,
/// extracting the mount point and `user`/`password` credentials from an
/// `Authorization: Basic <base64(user:pass)>` header.
///
/// Reads exactly the request line + headers -- does not consume any audio
/// payload bytes that follow the blank line. Pure and side-effect-free
/// beyond consuming `reader`, so it's used both by the real TCP connection
/// path (after buffering the header block off the socket -- see
/// `read_header_block`) and directly by this module's unit tests (via
/// `Cursor` over synthetic bytes).
pub fn parse_handshake<R: BufRead>(reader: &mut R) -> Result<HarborHandshake, String> {
    let mut request_line = String::new();
    reader
        .read_line(&mut request_line)
        .map_err(|e| format!("failed to read request line: {e}"))?;
    let request_line = request_line.trim_end_matches(['\r', '\n']);
    if request_line.is_empty() {
        return Err("empty request line".to_string());
    }

    let parts: Vec<&str> = request_line.split_whitespace().collect();
    if parts.len() < 2 {
        return Err(format!("malformed request line: '{request_line}'"));
    }
    let method = parts[0];
    if method != "SOURCE" && method != "PUT" {
        return Err(format!(
            "unsupported source method '{method}' (expected SOURCE or PUT -- legacy Shoutcast \
             framing is out of scope for this engine)"
        ));
    }
    let mount = parts[1].to_string();
    if !mount.starts_with('/') {
        return Err(format!(
            "malformed mount point '{mount}' (expected a leading '/')"
        ));
    }

    let mut headers: HashMap<String, String> = HashMap::new();
    loop {
        let mut line = String::new();
        let n = reader
            .read_line(&mut line)
            .map_err(|e| format!("failed to read header line: {e}"))?;
        if n == 0 {
            return Err("connection closed before headers completed".to_string());
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        match trimmed.split_once(':') {
            Some((k, v)) => {
                headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
            }
            None => return Err(format!("malformed header line: '{trimmed}'")),
        }
    }

    let auth_header = headers
        .get("authorization")
        .ok_or_else(|| "missing Authorization header".to_string())?;
    let b64 = auth_header
        .strip_prefix("Basic ")
        .ok_or_else(|| "Authorization header is not 'Basic <credentials>'".to_string())?;
    let decoded = STANDARD
        .decode(b64.trim())
        .map_err(|e| format!("failed to base64-decode Authorization header: {e}"))?;
    let decoded = String::from_utf8(decoded)
        .map_err(|e| format!("Authorization payload is not valid UTF-8: {e}"))?;
    let (user, password) = decoded
        .split_once(':')
        .ok_or_else(|| "Authorization payload missing ':' separator".to_string())?;

    Ok(HarborHandshake {
        mount,
        user: user.to_string(),
        password: password.to_string(),
        content_type: headers.get("content-type").cloned(),
    })
}

// ---------------------------------------------------------------------
// Shared live state
// ---------------------------------------------------------------------

struct LiveSession {
    /// Read via `LiveState::live_dj`/`live_dj_name` below -- both mirror
    /// SPEC.md C.4's `live_dj()`/`live_dj_name()` getters exactly, but
    /// nothing in this phase's engine has a concrete consumer for them yet
    /// (no status/now-playing endpoint surfaces the connected DJ's identity
    /// today). Kept as real, spec-correct public API rather than removed,
    /// same "spec'd but not yet wired to a caller" status as e.g.
    /// `callbacks.rs`'s `call_savecache`.
    #[allow(dead_code)]
    user: String,
    #[allow(dead_code)]
    display_name: String,
    /// A cloned handle to the live connection's socket, kept solely so
    /// `/streamer/disconnect` (SPEC.md's forced-disconnect path) can call
    /// `shutdown()` on it without needing any other coordination with the
    /// blocking decode thread that owns the "real" handle.
    closer: TcpStream,
}

/// Shared live-DJ harbor state. One instance is constructed in `main.rs`
/// (wrapped in `Arc`) and cloned into `pipeline.rs` (reads chunks, polls
/// the to-live transition edge), `server.rs` (`/streamer/disconnect`), and
/// this module's own connection-handling tasks (writes).
pub struct LiveState {
    ready: AtomicBool,
    to_live: AtomicBool,
    chunk_rx: TokioMutex<Option<mpsc::UnboundedReceiver<Vec<f32>>>>,
    session: StdMutex<Option<LiveSession>>,
}

impl LiveState {
    pub fn new() -> Self {
        Self {
            ready: AtomicBool::new(false),
            to_live: AtomicBool::new(false),
            chunk_rx: TokioMutex::new(None),
            session: StdMutex::new(None),
        }
    }

    /// `true` once the currently-connected DJ (if any) has produced at
    /// least one decoded chunk of audio -- see this module's doc for why
    /// readiness is gated on "producing audio" rather than bare TCP
    /// connect.
    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::SeqCst)
    }

    /// SPEC.md C.4's `live_dj()` -- stays populated with the outgoing DJ's
    /// identity until the async `djoff` callback actually completes (see
    /// `clear_session`'s call site in `handle_connection`). Not yet called
    /// from anywhere in this phase's engine (no status endpoint surfaces
    /// it) -- kept as real, spec-correct public API regardless, same as
    /// `live_dj_name` below.
    #[allow(dead_code)]
    pub fn live_dj(&self) -> Option<String> {
        self.session.lock().unwrap().as_ref().map(|s| s.user.clone())
    }

    /// SPEC.md C.4's `live_dj_name()`.
    #[allow(dead_code)]
    pub fn live_dj_name(&self) -> Option<String> {
        self.session
            .lock()
            .unwrap()
            .as_ref()
            .map(|s| s.display_name.clone())
    }

    fn has_session(&self) -> bool {
        self.session.lock().unwrap().is_some()
    }

    /// SPEC.md B.4 #3's `check_live()`, intended to be called once per
    /// `pipeline.rs` loop iteration: mirrors `azuracast.to_live`'s ref
    /// semantics (stays `true` continuously while ready, resets to `false`
    /// the instant readiness is lost), but returns whether *this call* is
    /// the one where the flag just flipped from false to true -- the edge
    /// `pipeline.rs` uses to decide whether to force an AutoDJ skip and to
    /// gate the to-live crossfade branch.
    pub fn poll_transition(&self) -> bool {
        if self.ready.load(Ordering::SeqCst) {
            !self.to_live.swap(true, Ordering::SeqCst)
        } else {
            self.to_live.store(false, Ordering::SeqCst);
            false
        }
    }

    /// Awaits and returns the next decoded live chunk as a `PreparedTrack`,
    /// or `None` if there is no active live session or the connection has
    /// just ended (channel closed) -- callers should fall back to
    /// requests/AutoDJ in either case, per SPEC.md C.8's priority order.
    pub async fn next_chunk(&self) -> Option<PreparedTrack> {
        let mut guard = self.chunk_rx.lock().await;
        let recv_result = match guard.as_mut() {
            Some(rx) => rx.recv().await,
            None => None,
        };
        match recv_result {
            Some(samples) => Some(prepare_live_chunk(DecodedTrack {
                samples,
                sample_rate: PIPELINE_SAMPLE_RATE,
                channels: PIPELINE_CHANNELS,
                replaygain_track_gain_db: None,
            })),
            None => {
                *guard = None;
                None
            }
        }
    }

    /// SPEC.md's `/streamer/disconnect` route (`input_streamer.stop`):
    /// forcibly closes the currently-live TCP connection, if any, which
    /// causes the blocking decode loop's next read to fail/EOF and run the
    /// exact same disconnect sequence (C.4) a voluntary client disconnect
    /// would -- there is deliberately no separate "forced disconnect" code
    /// path. Returns `false` if there was nothing to disconnect.
    pub fn force_disconnect(&self) -> bool {
        let session = self.session.lock().unwrap();
        match session.as_ref() {
            Some(s) => {
                if let Err(e) = s.closer.shutdown(Shutdown::Both) {
                    tracing::warn!("harbor: failed to shut down live connection: {e}");
                }
                true
            }
            None => false,
        }
    }

    async fn install_receiver(&self, rx: mpsc::UnboundedReceiver<Vec<f32>>) {
        *self.chunk_rx.lock().await = Some(rx);
    }

    fn set_session(&self, user: String, display_name: String, closer: TcpStream) {
        *self.session.lock().unwrap() = Some(LiveSession {
            user,
            display_name,
            closer,
        });
    }

    fn mark_ready(&self) {
        self.ready.store(true, Ordering::SeqCst);
    }

    /// Synchronous, immediate flip -- SPEC.md C.4: "`live_enabled` goes
    /// false immediately on disconnect", before the async `djoff` call even
    /// starts.
    fn mark_disconnected(&self) {
        self.ready.store(false, Ordering::SeqCst);
    }

    fn clear_session(&self) {
        *self.session.lock().unwrap() = None;
    }
}

impl Default for LiveState {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------
// TCP listener + connection handling
// ---------------------------------------------------------------------

/// Runs the harbor TCP listener forever (until process teardown), matching
/// the lifecycle of `pipeline.rs`'s loop. No-op (returns immediately,
/// without binding anything) if `cfg.enabled` is `false` -- SPEC.md B.4's
/// `!station->enable_streamers` no-op check.
pub async fn run_harbor_listener(cfg: HarborConfig, client: Arc<CallbackClient>, live: Arc<LiveState>) {
    if !cfg.enabled {
        tracing::info!("harbor (live DJ) listener disabled (harbor.enabled=false); not binding");
        return;
    }

    let ip_addr: std::net::IpAddr = match cfg.bind_address.parse() {
        Ok(ip) => ip,
        Err(e) => {
            tracing::error!(
                "invalid harbor.bind_address '{}': not a valid IPv4 or IPv6 address: {e}",
                cfg.bind_address
            );
            return;
        }
    };
    let bind_addr = SocketAddr::new(ip_addr, cfg.port);
    let listener = match tokio::net::TcpListener::bind(bind_addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("failed to bind harbor listener to {bind_addr}: {e}");
            return;
        }
    };
    tracing::info!(
        "harbor (live DJ) listener on {bind_addr}, mount '{}' (Icecast2/HTTP-style source \
         protocol only -- no legacy Shoutcast framing)",
        cfg.mount_point
    );
    if let Some(buffer_secs) = cfg.buffer_secs {
        // Logged for visibility only -- this engine's live decode path is
        // unbuffered/streaming by construction, so there is no separate
        // input-buffer depth to actually apply these to yet (see
        // `HarborConfig`'s doc comment).
        tracing::info!(
            "harbor: station configured buffer_secs={buffer_secs} max_buffer_secs={:?} \
             (not currently applied -- see HarborConfig's doc comment)",
            cfg.max_buffer_secs
        );
    }

    loop {
        let (socket, peer) = match listener.accept().await {
            Ok(x) => x,
            Err(e) => {
                tracing::warn!("harbor: accept error: {e}");
                continue;
            }
        };
        let cfg = cfg.clone();
        let client = client.clone();
        let live = live.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(socket, peer, cfg, client, live).await {
                tracing::info!("harbor: connection from {peer} ended: {e}");
            }
        });
    }
}

/// Reads raw request-line + header bytes off `reader` up to (and including)
/// the blank line terminating headers. Does not consume anything past that
/// blank line -- `handle_connection` recovers whatever the underlying
/// `tokio::io::BufReader` had already buffered past it (see its own doc)
/// before handing the raw socket off to the blocking decode phase.
async fn read_header_block(
    reader: &mut tokio::io::BufReader<tokio::net::TcpStream>,
) -> Result<String, String> {
    let mut text = String::new();
    loop {
        if text.len() > MAX_HEADER_BLOCK_BYTES {
            return Err("header block exceeded maximum size".to_string());
        }
        let mut line = String::new();
        let n = reader
            .read_line(&mut line)
            .await
            .map_err(|e| format!("failed to read from socket: {e}"))?;
        if n == 0 {
            return Err("connection closed before headers completed".to_string());
        }
        let is_blank = line == "\r\n" || line == "\n";
        text.push_str(&line);
        if is_blank {
            return Ok(text);
        }
    }
}

async fn write_response(socket: &mut tokio::net::TcpStream, status: u16, reason: &str) {
    let resp = format!("HTTP/1.0 {status} {reason}\r\n\r\n");
    if let Err(e) = socket.write_all(resp.as_bytes()).await {
        tracing::debug!("harbor: failed writing response: {e}");
    }
}

/// Handles one accepted source-client connection end to end: handshake,
/// mount/session checks, `auth` callback, `djon`/`djoff` sequencing (SPEC.md
/// C.4), and the streaming decode loop that feeds `LiveState`'s chunk
/// channel. Returns once the connection has fully ended (voluntarily,
/// forcibly via `/streamer/disconnect`, or due to a handshake/auth
/// rejection).
async fn handle_connection(
    socket: tokio::net::TcpStream,
    peer: SocketAddr,
    cfg: HarborConfig,
    client: Arc<CallbackClient>,
    live: Arc<LiveState>,
) -> Result<(), String> {
    let mut reader = tokio::io::BufReader::new(socket);

    let header_text = read_header_block(&mut reader).await?;
    let handshake = parse_handshake(&mut Cursor::new(header_text.as_bytes()))?;

    if handshake.mount != cfg.mount_point {
        write_response(reader.get_mut(), 404, "Not Found").await;
        return Err(format!(
            "rejected: mount '{}' does not match configured mount '{}'",
            handshake.mount, cfg.mount_point
        ));
    }

    if live.has_session() {
        write_response(reader.get_mut(), 403, "Mount In Use").await;
        return Err("rejected: a live session is already active on this mount".to_string());
    }

    let auth_payload = json!({ "user": handshake.user, "password": handshake.password });
    let auth = match client.call_auth(auth_payload).await {
        Ok(a) if a.allow => a,
        Ok(_) => {
            write_response(reader.get_mut(), 401, "Unauthorized").await;
            return Err(format!("auth rejected for user '{}'", handshake.user));
        }
        Err(e) => {
            write_response(reader.get_mut(), 401, "Unauthorized").await;
            return Err(format!("auth callback failed: {e}"));
        }
    };

    // SPEC.md C.4: `last_authenticated_dj := username ?? ''`,
    // `last_authenticated_dj_name := display_name ?? live_broadcast_text()`.
    let user = auth.username.unwrap_or_else(|| handshake.user.clone());
    let display_name = auth
        .display_name
        .unwrap_or_else(|| DEFAULT_LIVE_BROADCAST_TEXT.to_string());

    write_response(reader.get_mut(), 200, "OK").await;
    tracing::info!("harbor: '{user}' ({display_name}) connected from {peer}");

    // SPEC.md C.4: `djon` fires asynchronously (`thread.run(fast=false)`),
    // not blocking the start of live decode.
    {
        let client = client.clone();
        let user_for_djon = user.clone();
        tokio::spawn(async move {
            if let Err(e) = client.call_djon(&user_for_djon).await {
                tracing::warn!("harbor: djon callback failed: {e}");
            }
        });
    }

    // Recover whatever `reader`'s internal buffer already holds past the
    // header block (a source client that doesn't wait for our response
    // before streaming audio can easily land audio bytes in the same TCP
    // read that pulled in the final header line) before discarding the
    // async wrapper -- otherwise those bytes would be silently lost.
    let leftover = reader.buffer().to_vec();
    let socket = reader.into_inner();
    let std_socket = socket
        .into_std()
        .map_err(|e| format!("failed to convert to a blocking socket: {e}"))?;
    std_socket
        .set_nonblocking(false)
        .map_err(|e| format!("failed to set blocking mode: {e}"))?;
    // Bounds how long the blocking decode thread can be stuck waiting on a
    // silently-vanished peer (no clean FIN/RST) -- not part of SPEC.md's
    // literal contract, but cheap connection hygiene so one wedged DJ
    // connection can't tie up a blocking-pool thread forever.
    let _ = std_socket.set_read_timeout(Some(std::time::Duration::from_secs(60)));
    let closer = std_socket
        .try_clone()
        .map_err(|e| format!("failed to clone socket handle: {e}"))?;

    live.set_session(user.clone(), display_name, closer);

    let (tx, rx) = mpsc::unbounded_channel();
    live.install_receiver(rx).await;

    let content_type = handshake.content_type.clone();
    let live_for_decode = live.clone();
    let decode_result = tokio::task::spawn_blocking(move || {
        let chained = Cursor::new(leftover).chain(std_socket);
        run_live_decode(chained, content_type, tx, live_for_decode);
    })
    .await;

    // Disconnect sequence (SPEC.md C.4): flip readiness false
    // immediately/synchronously, *then* fire `djoff` asynchronously, only
    // clearing the session (live_dj/live_dj_name) once that completes.
    live.mark_disconnected();
    tracing::info!("harbor: '{user}' disconnected from {peer}");

    tokio::spawn(async move {
        if let Err(e) = client.call_djoff(&user).await {
            tracing::warn!("harbor: djoff callback failed: {e}");
        }
        live.clear_session();
    });

    decode_result.map_err(|e| format!("live decode task panicked: {e}"))
}

/// Streaming decode loop (blocking -- must run on a dedicated OS thread,
/// e.g. via `tokio::task::spawn_blocking`, since `symphonia`'s
/// `MediaSourceStream` performs synchronous reads against `reader`).
/// Decodes packet-by-packet as bytes arrive over the live connection,
/// resamples/remixes each decoded chunk to the pipeline's fixed format via
/// a persistent `StreamResampler` (so its internal delay line carries over
/// between chunks correctly), and pushes the result to `tx`. Returns
/// (silently) on clean EOF, a fatal decode/probe error, or once `tx`'s
/// receiver has been dropped (pipeline/connection replaced).
fn run_live_decode<R: Read + Send + Sync + 'static>(
    reader: R,
    content_type: Option<String>,
    tx: mpsc::UnboundedSender<Vec<f32>>,
    live: Arc<LiveState>,
) {
    let source = ReadOnlySource::new(reader);
    let mss = MediaSourceStream::new(Box::new(source), Default::default());

    let mut hint = Hint::new();
    if let Some(ct) = content_type.as_deref() {
        hint.mime_type(ct);
    }

    let probed = match symphonia::default::get_probe().format(
        &hint,
        mss,
        &FormatOptions::default(),
        &MetadataOptions::default(),
    ) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("harbor: failed to probe live stream: {e}");
            return;
        }
    };
    let mut format = probed.format;

    let track = match format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
    {
        Some(t) => t.clone(),
        None => {
            tracing::warn!("harbor: no decodable audio track found in live stream");
            return;
        }
    };
    let track_id = track.id;

    let mut decoder = match symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
    {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!("harbor: failed to create decoder for live stream: {e}");
            return;
        }
    };

    let mut resampler: Option<StreamResampler> = None;
    let mut announced_ready = false;

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymphoniaError::IoError(ref e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break
            }
            Err(SymphoniaError::ResetRequired) => break,
            Err(e) => {
                tracing::info!("harbor: live stream ended: {e}");
                break;
            }
        };
        if packet.track_id() != track_id {
            continue;
        }

        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(SymphoniaError::DecodeError(msg)) => {
                tracing::warn!("harbor: decode error in live stream (skipping packet): {msg}");
                continue;
            }
            Err(e) => {
                tracing::warn!("harbor: fatal decode error in live stream: {e}");
                break;
            }
        };

        let spec = *decoded.spec();
        let source_rate = spec.rate;
        let source_channels = spec.channels.count();
        if source_rate == 0 || source_channels == 0 {
            continue;
        }

        let mut interleaved = Vec::new();
        append_interleaved(&decoded, &mut interleaved);
        let planes = to_stereo_channel_planes(&interleaved, source_channels);

        if resampler.is_none() {
            resampler = match StreamResampler::new(
                source_rate,
                PIPELINE_SAMPLE_RATE,
                PIPELINE_CHANNELS as usize,
            ) {
                Ok(r) => Some(r),
                Err(e) => {
                    tracing::warn!("harbor: failed to initialize live resampler: {e}");
                    return;
                }
            };
        }

        let resampled = match resampler.as_mut().unwrap().process(planes) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("harbor: live resample error (dropping chunk): {e}");
                continue;
            }
        };
        if resampled.is_empty() || resampled[0].is_empty() {
            continue;
        }

        let out = interleave_stereo(&resampled);

        if !announced_ready {
            live.mark_ready();
            announced_ready = true;
            tracing::info!("harbor: live stream is producing decoded audio");
        }

        if tx.send(out).is_err() {
            tracing::info!("harbor: pipeline receiver dropped; ending live decode");
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn basic_auth_header(user: &str, pass: &str) -> String {
        STANDARD.encode(format!("{user}:{pass}"))
    }

    #[test]
    fn parses_source_request_with_basic_auth() {
        let auth = basic_auth_header("streamer1", "hunter2");
        let raw = format!(
            "SOURCE /live ICE/1.0\r\nAuthorization: Basic {auth}\r\nContent-Type: audio/mpeg\r\nUser-Agent: BUTT\r\n\r\n"
        );
        let mut cursor = Cursor::new(raw.as_bytes());
        let hs = parse_handshake(&mut cursor).expect("should parse");
        assert_eq!(hs.mount, "/live");
        assert_eq!(hs.user, "streamer1");
        assert_eq!(hs.password, "hunter2");
        assert_eq!(hs.content_type.as_deref(), Some("audio/mpeg"));
    }

    #[test]
    fn parses_put_request() {
        let auth = basic_auth_header("dj", "pw");
        let raw = format!("PUT /live HTTP/1.0\r\nAuthorization: Basic {auth}\r\n\r\n");
        let mut cursor = Cursor::new(raw.as_bytes());
        let hs = parse_handshake(&mut cursor).expect("should parse");
        assert_eq!(hs.mount, "/live");
        assert_eq!(hs.user, "dj");
        assert_eq!(hs.password, "pw");
        assert_eq!(hs.content_type, None);
    }

    #[test]
    fn rejects_unsupported_method() {
        let raw = b"GET /live HTTP/1.0\r\nAuthorization: Basic Zm9vOmJhcg==\r\n\r\n";
        let mut cursor = Cursor::new(&raw[..]);
        assert!(parse_handshake(&mut cursor).is_err());
    }

    #[test]
    fn rejects_missing_authorization_header() {
        let raw = b"SOURCE /live ICE/1.0\r\nContent-Type: audio/mpeg\r\n\r\n";
        let mut cursor = Cursor::new(&raw[..]);
        assert!(parse_handshake(&mut cursor).is_err());
    }

    #[test]
    fn rejects_non_basic_authorization() {
        let raw = b"SOURCE /live ICE/1.0\r\nAuthorization: Bearer sometoken\r\n\r\n";
        let mut cursor = Cursor::new(&raw[..]);
        assert!(parse_handshake(&mut cursor).is_err());
    }

    #[test]
    fn rejects_malformed_base64() {
        let raw = b"SOURCE /live ICE/1.0\r\nAuthorization: Basic not-valid-base64!!!\r\n\r\n";
        let mut cursor = Cursor::new(&raw[..]);
        assert!(parse_handshake(&mut cursor).is_err());
    }

    #[test]
    fn rejects_credentials_missing_colon() {
        let b64 = STANDARD.encode("nocolonhere");
        let raw = format!("SOURCE /live ICE/1.0\r\nAuthorization: Basic {b64}\r\n\r\n");
        let mut cursor = Cursor::new(raw.as_bytes());
        assert!(parse_handshake(&mut cursor).is_err());
    }

    #[test]
    fn rejects_mount_without_leading_slash() {
        let auth = basic_auth_header("u", "p");
        let raw = format!("SOURCE live ICE/1.0\r\nAuthorization: Basic {auth}\r\n\r\n");
        let mut cursor = Cursor::new(raw.as_bytes());
        assert!(parse_handshake(&mut cursor).is_err());
    }

    #[test]
    fn rejects_malformed_header_line() {
        let raw = b"SOURCE /live ICE/1.0\r\nNotAHeaderLine\r\n\r\n";
        let mut cursor = Cursor::new(&raw[..]);
        assert!(parse_handshake(&mut cursor).is_err());
    }

    #[test]
    fn rejects_truncated_connection_before_headers_complete() {
        let raw = b"SOURCE /live ICE/1.0\r\nAuthorization: Basic Zm9vOmJhcg==\r\n";
        let mut cursor = Cursor::new(&raw[..]);
        assert!(parse_handshake(&mut cursor).is_err());
    }

    #[test]
    fn live_state_poll_transition_fires_once_per_ready_period() {
        let live = LiveState::new();
        assert!(!live.poll_transition());
        live.mark_ready();
        assert!(live.poll_transition(), "first ready poll should be the edge");
        assert!(!live.poll_transition(), "subsequent polls while ready should not re-fire");
        live.mark_disconnected();
        assert!(!live.poll_transition());
        live.mark_ready();
        assert!(
            live.poll_transition(),
            "becoming ready again after a disconnect should re-fire the edge"
        );
    }
}
