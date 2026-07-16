//! Network output (Phase 5): encodes the pipeline's mixed PCM audio and
//! pushes it to the station's own local Icecast frontend (`[icecast_output]`
//! + `[[mounts]]`) and to zero or more third-party relay targets
//! (`[[remotes]]`), per `engine/SPEC.md` B.6-B.9/B.14. This is the outbound
//! mirror of `harbor.rs`'s inbound source-client handshake: `harbor.rs`
//! *accepts* a `SOURCE`/`PUT` request and parses it; this module *builds and
//! sends* one, then reads the target's response, exactly the other half of
//! the same Icecast2/HTTP-style protocol.
//!
//! ## Scope (see the Phase 5 task description for the full boundary)
//!
//! - **Protocol**: Icecast2 source-CLIENT only -- `SOURCE <mount> ICE/1.0`
//!   + `Authorization: Basic <base64(user:pass)>` + `Content-Type`, then the
//!   target's `200`-ish response line, then the encoded audio stream as the
//!   request body. No legacy Shoutcast1/2 framing (matching `harbor.rs`'s
//!   own inbound scope limitation) and no ICY protocol variants beyond this.
//! - **Encoding**: shells out to an `ffmpeg` subprocess (matching this
//!   project's established plan-level decision to use `ffmpeg` for
//!   encode/mux rather than reimplementing codecs in Rust) -- raw
//!   interleaved `f32` PCM at the pipeline's fixed rate/channel count piped
//!   in via stdin, encoded bytes read back out via stdout. Container per
//!   format: `mp3`->`mp3`, `aac`->`adts`, `ogg`->`ogg`, `opus`->`ogg` (Opus
//!   is conventionally carried in an Ogg container for source-client
//!   streaming; there is no separate raw-Opus container ffmpeg's muxers
//!   commonly emit here), `flac`->`flac`. AAC uses ffmpeg's built-in `aac`
//!   encoder rather than `libfdk_aac`: `libfdk_aac` is a non-default,
//!   often-not-compiled-in ffmpeg encoder (its license terms keep it out of
//!   most distро ffmpeg builds), so defaulting to the encoder that is
//!   actually present in a stock ffmpeg build is the safer choice; this is a
//!   documented judgment call, not an oversight.
//! - **One independent ffmpeg process + one independent outbound TCP
//!   connection per mount and per remote** -- deliberately no
//!   `share_encoders`-style single-shared-encoder optimization
//!   (`engine/SPEC.md` B.6's `share_encoders` is explicitly out of scope for
//!   this engine, matching other established simplifications elsewhere in
//!   this codebase). If a station has 3 mounts, that's 3 ffmpeg processes +
//!   3 Icecast connections, even when two mounts share identical
//!   format/bitrate.
//! - **Resilience**: each output target runs as its own independent,
//!   infinitely-retrying task (`run_output_target`) with a fixed reconnect
//!   backoff -- there is no historical Liquidsoap behavior to match here
//!   (this is new engine-side behavior with no SPEC.md-documented
//!   precedent), so a fixed delay is used rather than anything more
//!   elaborate. One target being unreachable never affects any other
//!   target, nor the AutoDJ/harbor pipeline itself.
//!
//! **Explicitly out of scope / deferred** (see the task report and
//! `README.md`): legacy Shoutcast/RSAS remote protocols (only
//! `protocol = "icecast"` is handled; anything else is logged and skipped),
//! `share_encoders`, and mid-stream ICY metadata updates pushed over an
//! already-open source connection (some source clients periodically
//! re-announce now-playing metadata this way -- noted here as a possible
//! future enhancement, not implemented). HLS output (SPEC.md B.8) was
//! originally deferred from this module's scope too, but is now implemented
//! separately in `hls.rs` -- see that module's doc.

use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::process::Command;
use tokio::sync::broadcast;

use crate::config::EngineConfig;
use crate::decode::{PIPELINE_CHANNELS, PIPELINE_SAMPLE_RATE};

/// Fixed reconnect backoff after any failure (connect, handshake rejection,
/// ffmpeg spawn failure, or the connection simply dropping). Not derived
/// from SPEC.md -- see this module's doc for why a fixed delay is
/// appropriate here.
const RECONNECT_BACKOFF: Duration = Duration::from_secs(5);

/// Upper bound on the whole connect-and-handshake exchange. Without this,
/// a server that accepts the TCP connection but never answers the SOURCE
/// request wedges the output task forever -- confirmed on a real install,
/// where Icecast 2.5-beta held a reconnecting source's request without
/// responding (its access log never even recorded the attempt) and the
/// engine sat "connecting" for minutes while the retry loop never fired.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);

/// Conventional default source-client username used by Icecast/Liquidsoap
/// when a station doesn't configure an explicit one (the local
/// `[icecast_output]` frontend has no username field at all -- source auth
/// there is password-only, by convention against the literal username
/// `"source"` -- and `[[remotes]]`'s `username` field is optional for the
/// same reason).
const DEFAULT_SOURCE_USERNAME: &str = "source";

/// The five station-configurable output formats (SPEC.md's `StreamFormats`
/// enum, as far as this engine implements it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Mp3,
    Aac,
    Ogg,
    Opus,
    Flac,
}

impl OutputFormat {
    /// Parses the `format` string PHP writes into `[[mounts]]`/`[[remotes]]`
    /// entries. Returns `None` for anything unrecognized -- callers should
    /// log a warning and skip that single output rather than treat it as a
    /// fatal config error (mirrors the unsupported-`protocol`-value
    /// handling for remotes).
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "mp3" => Some(OutputFormat::Mp3),
            "aac" => Some(OutputFormat::Aac),
            "ogg" => Some(OutputFormat::Ogg),
            "opus" => Some(OutputFormat::Opus),
            "flac" => Some(OutputFormat::Flac),
            _ => None,
        }
    }

    /// ffmpeg output container (`-f <container>`) -- see this module's doc
    /// for the mapping rationale (particularly Opus->Ogg).
    pub fn container(self) -> &'static str {
        match self {
            OutputFormat::Mp3 => "mp3",
            OutputFormat::Aac => "adts",
            OutputFormat::Ogg => "ogg",
            OutputFormat::Opus => "ogg",
            OutputFormat::Flac => "flac",
        }
    }

    /// `Content-Type` header sent in the outbound `SOURCE`/`PUT` request --
    /// matching what real source clients advertise for each format.
    pub fn content_type(self) -> &'static str {
        match self {
            OutputFormat::Mp3 => "audio/mpeg",
            OutputFormat::Aac => "audio/aac",
            OutputFormat::Ogg => "application/ogg",
            OutputFormat::Opus => "application/ogg",
            OutputFormat::Flac => "audio/flac",
        }
    }

    /// ffmpeg codec-selection + bitrate args for this format. FLAC is
    /// lossless and takes no bitrate arg at all -- `bitrate_kbps` is simply
    /// ignored in that case (the config still carries a `bitrate` field for
    /// FLAC entries per the fixed TOML contract, but this engine has
    /// nothing meaningful to do with it).
    pub fn codec_args(self, bitrate_kbps: u32) -> Vec<String> {
        match self {
            OutputFormat::Mp3 => vec![
                "-c:a".to_string(),
                "libmp3lame".to_string(),
                "-b:a".to_string(),
                format!("{bitrate_kbps}k"),
            ],
            OutputFormat::Aac => vec![
                "-c:a".to_string(),
                "aac".to_string(),
                "-b:a".to_string(),
                format!("{bitrate_kbps}k"),
            ],
            OutputFormat::Ogg => vec![
                "-c:a".to_string(),
                "libvorbis".to_string(),
                "-b:a".to_string(),
                format!("{bitrate_kbps}k"),
            ],
            OutputFormat::Opus => vec![
                "-c:a".to_string(),
                "libopus".to_string(),
                "-b:a".to_string(),
                format!("{bitrate_kbps}k"),
            ],
            OutputFormat::Flac => vec!["-c:a".to_string(), "flac".to_string()],
        }
    }
}

/// Full ffmpeg CLI argument list for encoding raw interleaved-`f32`-LE PCM
/// (read from stdin) to `format` at `bitrate_kbps`, writing the encoded
/// container bytes to stdout. Pure/unit-testable: builds the argument
/// vector without spawning anything.
pub fn ffmpeg_args(
    format: OutputFormat,
    bitrate_kbps: u32,
    sample_rate: u32,
    channels: u16,
) -> Vec<String> {
    let mut args = vec![
        "-hide_banner".to_string(),
        "-loglevel".to_string(),
        "error".to_string(),
        "-f".to_string(),
        "f32le".to_string(),
        "-ar".to_string(),
        sample_rate.to_string(),
        "-ac".to_string(),
        channels.to_string(),
        "-i".to_string(),
        "pipe:0".to_string(),
    ];
    args.extend(format.codec_args(bitrate_kbps));
    args.push("-f".to_string());
    args.push(format.container().to_string());
    args.push("pipe:1".to_string());
    args
}

/// One outbound Icecast source-client target -- either a local mount
/// (`[icecast_output]` + one `[[mounts]]` entry) or a remote relay (one
/// `[[remotes]]` entry), normalized to the same shape since the outbound
/// protocol/encode path is identical either way.
#[derive(Debug, Clone)]
pub struct IcecastTarget {
    pub host: String,
    pub port: u16,
    /// Always has a leading `/` (mount paths from PHP are written this way;
    /// `harbor.rs::parse_handshake` requires it on the inbound side too).
    pub mount: String,
    pub username: String,
    pub password: String,
    pub format: OutputFormat,
    pub bitrate: u32,
    pub is_public: bool,
    /// Human-readable label for log lines only (e.g. `"local mount
    /// /station.mp3"` or `"remote relay.example.com:8000/relay-mount"`).
    pub label: String,
}

/// Builds the exact outbound `SOURCE <mount> ICE/1.0` request (request line
/// + headers + terminating blank line) for `target` -- the direct outbound
/// counterpart of `harbor.rs::parse_handshake`. Deliberately built to be
/// accepted by that exact parser (see this module's tests, which round-trip
/// through it as a cross-check between the two halves of the protocol now
/// implemented in this codebase).
pub fn build_source_request(target: &IcecastTarget) -> Vec<u8> {
    let credentials = format!("{}:{}", target.username, target.password);
    let b64 = STANDARD.encode(credentials);
    let public_flag = if target.is_public { 1 } else { 0 };
    let request = format!(
        "SOURCE {mount} ICE/1.0\r\n\
         Authorization: Basic {b64}\r\n\
         Content-Type: {content_type}\r\n\
         User-Agent: azuracast-engine\r\n\
         ice-public: {public_flag}\r\n\
         \r\n",
        mount = target.mount,
        content_type = target.format.content_type(),
    );
    request.into_bytes()
}

/// `true` if `line` (the first line of the target's response, e.g. `"HTTP/1.0
/// 200 OK\r\n"` or `"ICY 200 OK\r\n"`) indicates a successful handshake --
/// i.e. it contains a bare `200` token. Pure/unit-testable independent of
/// any real network connection.
fn is_success_status_line(line: &str) -> bool {
    line.split_whitespace().any(|tok| tok == "200")
}

/// Connects to `target`, sends the source-client handshake, and reads back
/// the target's response headers (up to and including the blank line, or
/// just the status line if the peer doesn't send anything further -- some
/// Icecast servers respond with only `"HTTP/1.0 200 OK\r\n\r\n"`). Returns
/// the raw `TcpStream` (headers already consumed) ready to have encoded
/// audio bytes written to it, or an `Err` describing why the handshake
/// failed.
async fn connect_and_handshake(target: &IcecastTarget) -> Result<TcpStream, String> {
    let addr = format!("{}:{}", target.host, target.port);
    let mut stream = TcpStream::connect(&addr)
        .await
        .map_err(|e| format!("connect to {addr} failed: {e}"))?;

    let request = build_source_request(target);
    stream
        .write_all(&request)
        .await
        .map_err(|e| format!("failed to send source request to {addr}: {e}"))?;

    let mut reader = BufReader::new(stream);
    let mut status_line = String::new();
    reader
        .read_line(&mut status_line)
        .await
        .map_err(|e| format!("failed to read response from {addr}: {e}"))?;

    if !is_success_status_line(&status_line) {
        return Err(format!(
            "target {addr}{} rejected source request: {}",
            target.mount,
            status_line.trim()
        ));
    }

    // Drain any remaining header lines up to the blank line, if the peer
    // sent more than just the status line -- we don't care about their
    // content, only that we don't leave them sitting in the stream ahead of
    // the audio bytes we're about to start writing.
    loop {
        let mut line = String::new();
        let n = reader
            .read_line(&mut line)
            .await
            .map_err(|e| format!("failed reading response headers from {addr}: {e}"))?;
        if n == 0 || line == "\r\n" || line == "\n" {
            break;
        }
    }

    Ok(reader.into_inner())
}

/// Spawns the `ffmpeg` subprocess for `target`, wired for raw-PCM-in /
/// encoded-container-out over stdin/stdout, stderr discarded (ffmpeg is
/// chatty on stderr even at `-loglevel error` in some builds; nothing in
/// this engine consumes it, matching `harbor.rs`'s general "don't record
/// diagnostic noise nobody reads" posture).
fn build_ffmpeg_command(format: OutputFormat, bitrate_kbps: u32) -> Command {
    let mut cmd = Command::new("ffmpeg");
    cmd.args(ffmpeg_args(
        format,
        bitrate_kbps,
        PIPELINE_SAMPLE_RATE,
        PIPELINE_CHANNELS,
    ));
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::null());
    cmd
}

/// Runs `target`'s output forever: connect, handshake, spawn ffmpeg, pump
/// the pipeline's tapped PCM into it, stream the encoded bytes out to the
/// Icecast connection -- and on any failure (at any stage), log it, wait
/// `RECONNECT_BACKOFF`, and try again from the top. Never returns; intended
/// to be spawned as its own independent `tokio::spawn` task per mount/remote
/// (see `main.rs`), so one target's outage never affects any other target
/// or the AutoDJ/harbor pipeline itself.
pub async fn run_output_target(target: IcecastTarget, tap: broadcast::Sender<Arc<Vec<f32>>>) {
    loop {
        match run_output_once(&target, &tap).await {
            Ok(()) => tracing::info!(
                "output[{}]: connection ended; reconnecting in {RECONNECT_BACKOFF:?}",
                target.label
            ),
            Err(e) => tracing::warn!(
                "output[{}]: {e}; reconnecting in {RECONNECT_BACKOFF:?}",
                target.label
            ),
        }
        tokio::time::sleep(RECONNECT_BACKOFF).await;
    }
}

/// One connect-handshake-encode-stream attempt for `target`. Returns once
/// the connection ends (cleanly or otherwise) -- the caller (`run_output_target`)
/// handles retry/backoff.
async fn run_output_once(
    target: &IcecastTarget,
    tap: &broadcast::Sender<Arc<Vec<f32>>>,
) -> Result<(), String> {
    let mut rx = tap.subscribe();

    tracing::info!(
        "output[{}]: connecting to {}:{}{}",
        target.label,
        target.host,
        target.port,
        target.mount
    );
    let stream = tokio::time::timeout(HANDSHAKE_TIMEOUT, connect_and_handshake(target))
        .await
        .map_err(|_| {
            format!(
                "handshake with {}:{} timed out after {HANDSHAKE_TIMEOUT:?}",
                target.host, target.port
            )
        })??;
    tracing::info!("output[{}]: connected, streaming", target.label);

    let mut child = build_ffmpeg_command(target.format, target.bitrate)
        .spawn()
        .map_err(|e| format!("failed to spawn ffmpeg: {e}"))?;

    let mut ffmpeg_stdin = child
        .stdin
        .take()
        .ok_or_else(|| "ffmpeg child has no stdin handle".to_string())?;
    let mut ffmpeg_stdout = child
        .stdout
        .take()
        .ok_or_else(|| "ffmpeg child has no stdout handle".to_string())?;

    // Feeds tapped PCM samples into ffmpeg's stdin as raw f32 LE bytes.
    // Runs as its own task so it can proceed independently of the
    // stdout->TCP copy below (ffmpeg buffers internally between the two).
    let feed_task = tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(samples) => {
                    let mut bytes = Vec::with_capacity(samples.len() * 4);
                    for s in samples.iter() {
                        bytes.extend_from_slice(&s.to_le_bytes());
                    }
                    if ffmpeg_stdin.write_all(&bytes).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(
                        "output feed lagged behind the pipeline by {n} message(s); \
                         continuing with newer audio"
                    );
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
        // Dropping `ffmpeg_stdin` here closes ffmpeg's stdin, signaling EOF
        // so it flushes and exits cleanly if the tap itself ever closes.
    });

    let mut stream = stream;
    let copy_result = tokio::io::copy(&mut ffmpeg_stdout, &mut stream).await;

    // Whichever side ended first (feed loop, ffmpeg exiting, or the TCP
    // connection dropping), tear down the other half before returning so
    // the next retry starts from a clean slate.
    feed_task.abort();
    let _ = child.kill().await;

    copy_result
        .map(|_| ())
        .map_err(|e| format!("stream copy to {} ended: {e}", target.label))
}

/// Builds the full list of outbound `IcecastTarget`s from the parsed
/// config: one per `[[mounts]]` entry (only if `[icecast_output]` is also
/// present) plus one per `[[remotes]]` entry with `protocol = "icecast"`.
/// Unrecognized formats/protocols are logged and that single entry is
/// skipped rather than failing the whole config -- see this module's doc
/// and the individual config field docs in `config.rs`.
pub fn build_targets(cfg: &EngineConfig) -> Vec<IcecastTarget> {
    let mut targets = Vec::new();

    match &cfg.icecast_output {
        Some(icecast) => {
            for (idx, mount) in cfg.mounts.iter().enumerate() {
                match OutputFormat::parse(&mount.format) {
                    Some(format) => targets.push(IcecastTarget {
                        host: icecast.host.clone(),
                        port: icecast.port,
                        mount: mount.path.clone(),
                        username: DEFAULT_SOURCE_USERNAME.to_string(),
                        password: icecast.source_password.clone(),
                        format,
                        bitrate: mount.bitrate,
                        is_public: mount.is_public,
                        label: format!("local mount #{} {}", idx + 1, mount.path),
                    }),
                    None => tracing::warn!(
                        "mount '{}' has unrecognized format '{}'; skipping this mount",
                        mount.path,
                        mount.format
                    ),
                }
            }
        }
        None => {
            if !cfg.mounts.is_empty() {
                tracing::warn!(
                    "{} mount(s) configured but no [icecast_output] section present; \
                     skipping all local mounts",
                    cfg.mounts.len()
                );
            }
        }
    }

    for (idx, remote) in cfg.remotes.iter().enumerate() {
        if !remote.protocol.eq_ignore_ascii_case("icecast") {
            tracing::warn!(
                "remote #{} ({}) uses unsupported protocol '{}' (only 'icecast' is \
                 implemented by this engine); skipping",
                idx + 1,
                remote.host,
                remote.protocol
            );
            continue;
        }
        match OutputFormat::parse(&remote.format) {
            Some(format) => targets.push(IcecastTarget {
                host: remote.host.clone(),
                port: remote.port,
                mount: remote.mount.clone(),
                username: remote
                    .username
                    .clone()
                    .unwrap_or_else(|| DEFAULT_SOURCE_USERNAME.to_string()),
                password: remote.password.clone(),
                format,
                bitrate: remote.bitrate,
                is_public: remote.is_public,
                label: format!(
                    "remote relay #{} {}:{}{}",
                    idx + 1,
                    remote.host,
                    remote.port,
                    remote.mount
                ),
            }),
            None => tracing::warn!(
                "remote #{} ({}) has unrecognized format '{}'; skipping",
                idx + 1,
                remote.host,
                remote.format
            ),
        }
    }

    targets
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harbor::parse_handshake;
    use std::io::Cursor;

    fn test_target(format: OutputFormat) -> IcecastTarget {
        IcecastTarget {
            host: "127.0.0.1".to_string(),
            port: 8000,
            mount: "/station.mp3".to_string(),
            username: "source".to_string(),
            password: "hackme".to_string(),
            format,
            bitrate: 128,
            is_public: true,
            label: "test".to_string(),
        }
    }

    // --- Handshake request construction -----------------------------------

    #[test]
    fn source_request_is_accepted_by_harbors_own_handshake_parser() {
        // Cross-check between the two halves of the protocol: whatever we
        // build here must be exactly what `harbor.rs::parse_handshake`
        // (the inbound/server side) successfully accepts.
        let target = test_target(OutputFormat::Mp3);
        let bytes = build_source_request(&target);
        let mut cursor = Cursor::new(bytes);
        let hs = parse_handshake(&mut cursor).expect("harbor parser should accept our request");
        assert_eq!(hs.mount, "/station.mp3");
        assert_eq!(hs.user, "source");
        assert_eq!(hs.password, "hackme");
        assert_eq!(hs.content_type.as_deref(), Some("audio/mpeg"));
    }

    #[test]
    fn source_request_carries_correct_content_type_per_format() {
        let cases = [
            (OutputFormat::Mp3, "audio/mpeg"),
            (OutputFormat::Aac, "audio/aac"),
            (OutputFormat::Ogg, "application/ogg"),
            (OutputFormat::Opus, "application/ogg"),
            (OutputFormat::Flac, "audio/flac"),
        ];
        for (format, expected_ct) in cases {
            let target = test_target(format);
            let bytes = build_source_request(&target);
            let mut cursor = Cursor::new(bytes);
            let hs = parse_handshake(&mut cursor).unwrap();
            assert_eq!(hs.content_type.as_deref(), Some(expected_ct));
        }
    }

    #[test]
    fn remote_with_no_username_falls_back_to_source_convention() {
        let remote = crate::config::RemoteConfig {
            host: "relay.example.com".to_string(),
            port: 8000,
            mount: "/relay-mount".to_string(),
            username: None,
            password: "hackme".to_string(),
            format: "mp3".to_string(),
            bitrate: 128,
            is_public: true,
            protocol: "icecast".to_string(),
        };
        let cfg = crate::config::parse_config(
            r#"
            [station]
            id = 1
            name = "Test"
            [control_api]
            bind_address = "127.0.0.1"
            port = 5000
            api_key = "k"
            [callbacks]
            base_url = "http://127.0.0.1"
            api_key = "k"
            station_id = 1
            [paths]
            log_file = "engine.log"
            "#,
        )
        .unwrap();
        let mut cfg = cfg;
        cfg.remotes = vec![remote];
        let targets = build_targets(&cfg);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].username, "source");
    }

    // --- Response status-line parsing --------------------------------------

    #[test]
    fn accepts_http_1_0_200_ok() {
        assert!(is_success_status_line("HTTP/1.0 200 OK\r\n"));
    }

    #[test]
    fn accepts_icy_200_ok() {
        assert!(is_success_status_line("ICY 200 OK\r\n"));
    }

    #[test]
    fn rejects_non_200_status() {
        assert!(!is_success_status_line("HTTP/1.0 401 Unauthorized\r\n"));
        assert!(!is_success_status_line("HTTP/1.0 403 Mount In Use\r\n"));
    }

    // --- ffmpeg argument construction ---------------------------------------

    #[test]
    fn ffmpeg_args_mp3_uses_libmp3lame_and_mp3_container() {
        let args = ffmpeg_args(OutputFormat::Mp3, 128, 44100, 2);
        assert!(args.windows(2).any(|w| w == ["-c:a", "libmp3lame"]));
        assert!(args.windows(2).any(|w| w == ["-b:a", "128k"]));
        assert!(args.windows(2).any(|w| w == ["-f", "mp3"]));
        assert!(args.windows(2).any(|w| w == ["-ar", "44100"]));
        assert!(args.windows(2).any(|w| w == ["-ac", "2"]));
        assert_eq!(args.last().unwrap(), "pipe:1");
    }

    #[test]
    fn ffmpeg_args_aac_uses_adts_container() {
        let args = ffmpeg_args(OutputFormat::Aac, 160, 44100, 2);
        assert!(args.windows(2).any(|w| w == ["-c:a", "aac"]));
        assert!(args.windows(2).any(|w| w == ["-f", "adts"]));
    }

    #[test]
    fn ffmpeg_args_ogg_uses_libvorbis_and_ogg_container() {
        let args = ffmpeg_args(OutputFormat::Ogg, 128, 44100, 2);
        assert!(args.windows(2).any(|w| w == ["-c:a", "libvorbis"]));
        assert!(args.windows(2).any(|w| w == ["-f", "ogg"]));
    }

    #[test]
    fn ffmpeg_args_opus_uses_libopus_and_ogg_container() {
        let args = ffmpeg_args(OutputFormat::Opus, 96, 44100, 2);
        assert!(args.windows(2).any(|w| w == ["-c:a", "libopus"]));
        assert!(args.windows(2).any(|w| w == ["-f", "ogg"]));
    }

    #[test]
    fn ffmpeg_args_flac_has_no_bitrate_flag() {
        let args = ffmpeg_args(OutputFormat::Flac, 0, 44100, 2);
        assert!(args.windows(2).any(|w| w == ["-c:a", "flac"]));
        assert!(args.windows(2).any(|w| w == ["-f", "flac"]));
        assert!(!args.iter().any(|a| a == "-b:a"));
    }

    #[test]
    fn output_format_parse_is_case_insensitive_and_rejects_unknown() {
        assert_eq!(OutputFormat::parse("MP3"), Some(OutputFormat::Mp3));
        assert_eq!(OutputFormat::parse("Opus"), Some(OutputFormat::Opus));
        assert_eq!(OutputFormat::parse("wma"), None);
    }

    // --- build_targets dispatch/skip logic ----------------------------------

    fn base_config_toml() -> &'static str {
        r#"
        [station]
        id = 1
        name = "Test"
        [control_api]
        bind_address = "127.0.0.1"
        port = 5000
        api_key = "k"
        [callbacks]
        base_url = "http://127.0.0.1"
        api_key = "k"
        station_id = 1
        [paths]
        log_file = "engine.log"
        "#
    }

    #[test]
    fn mounts_without_icecast_output_are_skipped() {
        let mut cfg = crate::config::parse_config(base_config_toml()).unwrap();
        cfg.mounts = vec![crate::config::MountConfig {
            path: "/station.mp3".to_string(),
            format: "mp3".to_string(),
            bitrate: 128,
            is_public: true,
        }];
        let targets = build_targets(&cfg);
        assert!(targets.is_empty());
    }

    #[test]
    fn unsupported_remote_protocol_is_skipped_not_fatal() {
        let mut cfg = crate::config::parse_config(base_config_toml()).unwrap();
        cfg.remotes = vec![crate::config::RemoteConfig {
            host: "relay.example.com".to_string(),
            port: 8000,
            mount: "/relay".to_string(),
            username: None,
            password: "pw".to_string(),
            format: "mp3".to_string(),
            bitrate: 128,
            is_public: true,
            protocol: "shoutcast".to_string(),
        }];
        let targets = build_targets(&cfg);
        assert!(targets.is_empty());
    }

    #[test]
    fn valid_mount_and_remote_both_produce_targets() {
        let mut cfg = crate::config::parse_config(base_config_toml()).unwrap();
        cfg.icecast_output = Some(crate::config::IcecastOutputConfig {
            host: "127.0.0.1".to_string(),
            port: 8000,
            source_password: "hackme".to_string(),
        });
        cfg.mounts = vec![crate::config::MountConfig {
            path: "/station.mp3".to_string(),
            format: "mp3".to_string(),
            bitrate: 128,
            is_public: true,
        }];
        cfg.remotes = vec![crate::config::RemoteConfig {
            host: "relay.example.com".to_string(),
            port: 8000,
            mount: "/relay".to_string(),
            username: Some("relayuser".to_string()),
            password: "pw".to_string(),
            format: "opus".to_string(),
            bitrate: 96,
            is_public: false,
            protocol: "icecast".to_string(),
        }];
        let targets = build_targets(&cfg);
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].mount, "/station.mp3");
        assert_eq!(targets[0].username, "source");
        assert_eq!(targets[1].mount, "/relay");
        assert_eq!(targets[1].username, "relayuser");
        assert_eq!(targets[1].format, OutputFormat::Opus);
    }

    // --- Real ffmpeg integration check (only if ffmpeg is on PATH) ---------

    /// A genuine, if narrow, end-to-end check: if `ffmpeg` is actually
    /// available in this environment, encode one second of synthetic silent
    /// PCM to MP3 and confirm we get back non-empty, plausible-looking
    /// output bytes. Skips (rather than failing or fabricating success) if
    /// `ffmpeg` isn't found -- see the task report for why this can't be
    /// guaranteed available in CI/dev environments.
    #[tokio::test]
    async fn encodes_synthetic_pcm_via_real_ffmpeg_if_available() {
        let has_ffmpeg = Command::new("ffmpeg")
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false);

        if !has_ffmpeg {
            eprintln!("ffmpeg not found on PATH; skipping real-encode integration check");
            return;
        }

        let mut child = build_ffmpeg_command(OutputFormat::Mp3, 128)
            .spawn()
            .expect("ffmpeg should spawn");

        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = child.stdout.take().unwrap();

        // One second of silence at the pipeline's fixed rate/channels.
        let frames = PIPELINE_SAMPLE_RATE as usize;
        let pcm = vec![0.0f32; frames * PIPELINE_CHANNELS as usize];
        let mut bytes = Vec::with_capacity(pcm.len() * 4);
        for s in &pcm {
            bytes.extend_from_slice(&s.to_le_bytes());
        }

        let write_task = tokio::spawn(async move {
            let _ = stdin.write_all(&bytes).await;
            // Drop stdin here to signal EOF to ffmpeg.
        });

        use tokio::io::AsyncReadExt;
        let mut encoded = Vec::new();
        stdout
            .read_to_end(&mut encoded)
            .await
            .expect("should read encoded output");

        write_task.await.unwrap();
        let _ = child.wait().await;

        assert!(!encoded.is_empty(), "ffmpeg should have produced encoded MP3 bytes");
        // A real MP3 stream either starts with an ID3 tag ("ID3") or an
        // MPEG frame sync (0xFFE... top 11 bits set).
        let looks_like_mp3 = encoded.starts_with(b"ID3")
            || (encoded.len() >= 2 && encoded[0] == 0xFF && (encoded[1] & 0xE0) == 0xE0);
        assert!(looks_like_mp3, "output should look like a valid MP3 stream");
    }
}
