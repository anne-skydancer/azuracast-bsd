# azuracast-engine

A Rust replacement for AzuraCast's Liquidsoap (OCaml) streaming engine, built in phases. See
`SPEC.md` in this directory for the full behavioral specification of the Liquidsoap integration
this engine is replacing.

**This is Phase 5: Icecast/Shoutcast source-client output.** Phase 2 built the skeleton (process
lifecycle, control API, callback client). Phase 3 added the real AutoDJ decode/crossfade playback
pipeline (annotation-string parsing, `media:`/`cp` resolution, full decode via `symphonia`,
autocue/replaygain, priority request queues, `cross.smart` crossfade dispatch, `feedback` dedup,
and a fallback/error-file safety net). Phase 4 added the live-broadcast ingestion path: a TCP
listener (`src/harbor.rs`) that accepts an Icecast2/HTTP-style source-client connection,
authenticates it against PHP (`auth`), decodes its live audio stream incrementally (chunked, not
full-buffer, via `symphonia`'s unseekable `MediaSource` support), and gives it playback priority
over AutoDJ, with `djon`/`djoff` sequencing and the crossfade "to-live" special case (SPEC.md C.5
point 1) wired up. Phase 5 adds the actual network output path (`src/output.rs`): the pipeline's
mixed PCM audio is now encoded (via an `ffmpeg` subprocess) and pushed out, as an Icecast2
source-client, to the station's own local Icecast frontend (once per configured mount) and to any
configured third-party relay targets — independently, one encoder + one connection per output. The
local raw-PCM file sink from Phase 3 is unchanged and still available alongside these real network
outputs. **Post-cutover** (after Liquidsoap/Docker were fully removed from this fork), HLS output
(SPEC.md B.8, originally deferred from Phase 5) was implemented in `src/hls.rs`: one independent
ffmpeg segmenter per configured `StationHlsStream`, writing `.ts` segments + a per-rendition
`.m3u8` directly to `station->getRadioHlsDir()` (file-based, not a network protocol -- nginx serves
that directory as-is, unchanged), plus a multi-bitrate master `live.m3u8` when more than one
rendition is configured. See the bottom of this file for what's still deliberately out of scope
(`share_encoders`, legacy Shoutcast relay protocols, mid-stream ICY metadata updates).

## Building

```sh
cargo build --release
```

The binary is produced at `target/release/azuracast-engine` (`azuracast-engine.exe` on Windows).

Dependencies (beyond Phase 2's `tokio`/`axum`/`reqwest`/`serde`/`toml`/`tracing`): `symphonia`
(pure-Rust decode: MP3, AAC/ADTS+MP4, OGG/Vorbis, FLAC, WAV/PCM), `rubato` (sample-rate
conversion), `ebur128` (loudness measurement for crossfade dB-level comparisons), `hound` (WAV
read/write, used only by the `--crossfade-test` CLI path), `base64` (harbor's inbound
`Authorization: Basic` decode, Phase 4, and output's outbound encode, Phase 5).

**External runtime dependency, new in Phase 5: `ffmpeg` must be present on `PATH` on the deployment
host.** `src/output.rs` shells out to an `ffmpeg` subprocess to encode the pipeline's raw PCM to
MP3/AAC/Ogg Vorbis/Opus/FLAC (matching this project's established plan-level decision to use
`ffmpeg` for encode/mux rather than reimplementing codecs in Rust) — this is not a Cargo
dependency and is not vendored; if `ffmpeg` is missing, every configured output target's ffmpeg
spawn fails, is logged, and that target retries on the usual backoff (see "Network output" below)
without affecting any other target or the AutoDJ/harbor pipeline itself.

> **Windows/OneDrive note:** if `cargo build`/`check` fails with `failed to link or copy ...
> Access is denied (os error 5)` on a `build-script-build.exe` under `target/`, this is OneDrive
> (or an antivirus scanner) transiently locking freshly-written executables inside a
> cloud-synced folder — not a real build error. Point `CARGO_TARGET_DIR` at a folder outside the
> synced tree (e.g. a temp directory) and retry; do not disable antivirus or otherwise work
> around this at the OS level.

## Running

```sh
azuracast-engine --config /path/to/engine.toml
```

Runs in the foreground (no daemonizing/forking) — this is intentional, since supervisord is
expected to manage the process directly (`autorestart`, signal-based stop). Logs go to stdout;
`paths.log_file` from the config is not yet written to directly in this phase (supervisord can
redirect stdout to that path itself). The process shuts down cleanly on SIGINT or SIGTERM,
logging `shutting down` before exiting 0.

Other modes:

```sh
azuracast-engine --check-config -     # reads a TOML document from stdin, validates it against
                                       # the config structs, exits 0 (valid) or non-zero with an
                                       # error on stderr (invalid). No server is started.
azuracast-engine --version            # prints "azuracast-engine 0.1.0" and exits 0.
azuracast-engine --crossfade-test <file1> <file2> --duration <seconds> --mode <smart|normal|disabled> --output <output.wav>
                                       # decodes two local audio files, runs them through the real
                                       # crossfade logic (src/crossfade.rs) with the given
                                       # mode/duration, and writes the resulting mix to a WAV file
                                       # via `hound` -- no control API, callbacks, or network
                                       # access required. Concrete A/B listening test for the
                                       # crossfade math. `--duration` defaults to 2.0s if omitted;
                                       # `--mode` defaults to "smart".
```

## Config file shape (`engine.toml`)

Written by the PHP side; treated here as a fixed contract (see `src/config.rs` for the exact
`serde` structs):

```toml
[station]
id = 1
name = "Example Station"

[control_api]
bind_address = "127.0.0.1"
port = 5000
api_key = "the-station-adapter-api-key"

# `bind_address` is a bare IP literal, parsed as either an IPv4 or IPv6
# address (no brackets, no port -- port is always the separate `port` field
# above). IPv6 is fully supported, e.g.:
#   bind_address = "::1"                        # IPv6 loopback
#   bind_address = "2001:8a0:6a32:2100::110"     # IPv6 station address

[callbacks]
base_url = "http://127.0.0.1:6010"
api_key = "the-station-adapter-api-key"
station_id = 1

[paths]
log_file = "/var/azuracast/stations/1/config/engine.log"
# New in Phase 3, both optional -- entirely absent is valid and falls back
# to documented degraded behavior (see below).
fallback_file_path = "/usr/local/share/icecast/web/error.mp3"
pipeline_output_path = "/var/azuracast/stations/1/engine-output.pcm"

# New in Phase 3, optional -- entirely absent uses the defaults shown.
# PHP's ConfigWriter/StreamEngine::getCurrentConfiguration() does not
# populate this section yet (see "Known follow-ups" below).
[crossfade]
mode = "normal"        # "smart" | "normal" | "disabled"/"none"
fade_seconds = 2.0      # SPEC.md A.1 `crossfade` (station default_fade)
high = -15.0            # SPEC.md A.1 `crossfade_smart_high`
medium = -32.0          # SPEC.md A.1 `crossfade_smart_medium`
margin = 8.0            # SPEC.md A.1 `crossfade_smart_margin`

# New in Phase 4, optional -- entirely absent uses the defaults shown
# (`enabled = false`, i.e. the harbor listener doesn't bind at all,
# matching SPEC.md B.4's `!station->enable_streamers` no-op).
[harbor]
enabled = true
bind_address = "0.0.0.0"
port = 8005
mount_point = "/"
charset = "UTF-8"
buffer_secs = 5.0        # optional -- only present when the station sets a non-zero DJ buffer
max_buffer_secs = 10.0   # optional -- only present alongside buffer_secs

# New in Phase 5, all optional -- entirely absent means no network output at
# all (the pipeline still runs, decoding/mixing/crossfading as normal, and
# still writes to `paths.pipeline_output_path` if that's configured; it just
# has nowhere else to send the audio). See "Network output" below for what's
# implemented vs. deferred.

# Present only if the station has a local Icecast frontend at all.
[icecast_output]
host = "127.0.0.1"
port = 8000
source_password = "hackme"

# Zero or more, only meaningful alongside [icecast_output]. One independent
# ffmpeg encoder + one independent Icecast connection per entry.
[[mounts]]
path = "/station.mp3"
format = "mp3"            # "mp3" | "aac" | "ogg" | "opus" | "flac"
bitrate = 128
is_public = true

# Zero or more, independent of [icecast_output] -- a station can relay to
# third-party servers with or without also running its own local frontend.
[[remotes]]
host = "relay.example.com"
port = 8000
mount = "/relay-mount"
username = "source"      # optional -- omitted entirely if the station has no explicit source username
password = "hackme"
format = "mp3"
bitrate = 128
is_public = true
protocol = "icecast"      # only "icecast" is implemented -- see "Network output" below
```

`buffer_secs`/`max_buffer_secs` are parsed and logged but not currently acted on -- this engine's
live decode path is unbuffered/streaming by construction (see "Harbor (live-DJ input)" below), so
there's no separate input-buffer depth to tune yet.

`[station]` also gained one new optional field in Phase 3:

```toml
[station]
id = 1
name = "Example Station"
replaygain_enabled = false   # SPEC.md A.1 `enable_replaygain_metadata`; defaults false, and PHP
                              # doesn't populate this yet either (see "Known follow-ups").
```

## Control API (inbound — PHP calls into the engine)

Binds to `{control_api.bind_address}:{control_api.port}`. Every route below requires header
`X-Engine-Api-Key: {control_api.api_key}`; a missing/mismatched key gets `401 Unauthorized` with
body `{"error": "unauthorized"}`. `GET /health` is the only unauthenticated route (basic
liveness probing, not part of the control contract).

| Method | Path | Body | Behavior |
|---|---|---|---|
| GET | `/health` | — | `200 {"status": "ok"}`, no auth required. |
| POST | `/skip` | — | **Real.** Signals `pipeline.rs`'s loop (via the shared `ControlSignals` handle in `src/control.rs`) to abandon the rest of the currently-playing track's body on its next loop check and jump straight to the crossfade into the next track, as if the body had naturally ended there -- SPEC.md C.9's `add_skip_command`/`source.skip(s)`. `200 {"ok": true}` is returned immediately (fire-and-forget); the merge/skip itself happens on the pipeline's next loop check, so there's a small "not instantaneous" polling delay, but as of Phase 6's real-time output pacing (see `pipeline.rs`'s module doc), once applied it *is* an audible real-time cut for a live listener, not just "the next produced chunk jumps to the crossfade". |
| POST | `/queue/{queue}/push` | `{"uri": "<string>"}` | **Real in Phase 3.** Pushes onto the actual in-memory `requests`/`interrupting_requests` queue (`src/queue.rs`), which `pipeline.rs` pops from in priority order ahead of AutoDJ. Unrecognized `{queue}` names get `400 {"ok": false, "error": "..."}` instead of the old silent-log behavior. |
| GET | `/queue/{queue}/empty` | — | **Real in Phase 3.** Reports the real queue's emptiness; unrecognized queue names report `true` (nothing to report). |
| POST | `/metadata` | JSON object, string values (`HashMap<String, String>`) | **Real.** Stages the given map (via the same `ControlSignals` handle) as an override to be merged onto the currently-playing track's metadata and re-pushed through the same `FeedbackDedup::maybe_send` dedup-and-push path every other metadata change uses -- SPEC.md C.9's `add_custom_metadata_command`/`insert_metadata`. `200 {"ok": true}` is returned immediately; the merge/re-push happens on the pipeline's next loop check, same "not instantaneous" caveat as `/skip`. Only the six fields `feedback`/C.6 forwards (`title`, `artist`, `song_id`, `media_id`, `sq_id`, `playlist_id`) are recognized -- other keys are accepted but ignored. |
| POST | `/streamer/disconnect` | — | **Real as of Phase 4.** Forcibly closes the currently-live harbor TCP connection, if any (`harbor::LiveState::force_disconnect`), which triggers the same disconnect sequence (SPEC.md C.4) a voluntary client disconnect would -- there is no separate "forced disconnect" code path. `200 {"ok": true}` is returned regardless of whether a connection was actually present. |

Every route in this table now has a real effect (as of Phase 4).

## Callback client (outbound — engine calls into PHP)

Implemented in `src/callbacks.rs`, one function per callback documented in `SPEC.md` section D
(`D.0` for the shared envelope, `D.1`-`D.7` for each command). Extracted contract:

- **URL**: `{callbacks.base_url}/api/internal/{callbacks.station_id}/liquidsoap/{action}`, where
  `action` is one of `nextsong`, `auth`, `djon`, `djoff`, `feedback`, `cp`, `savecache`.
- **Method**: always `POST` (D.0 documents the route as `GET|POST`, but the real Liquidsoap-side
  caller — `azuracast.api_call`, SPEC.md C.2 — always uses `http.post`, so this client does too).
- **Auth**: header `X-Liquidsoap-Api-Key: {callbacks.api_key}`.
- **Other headers**: `Content-Type: application/json`, `User-Agent: Liquidsoap AzuraCast`.
- **Body**: JSON-stringified payload for every callback except `nextsong`, which sends a literal
  empty string (no JSON at all — see D.1).
- **Response handling**: HTTP 200 is success (body parsed per-callback); any non-200 status is
  treated as a hard failure and surfaced as an `Err` — this engine surfaces/logs those errors
  rather than silently returning `null` the way `azuracast.api_call` does, since there's a real
  caller here that wants to know.

| Function | Callback | Payload | Timeout | Response |
|---|---|---|---|---|
| `call_nextsong` | `nextsong` | `""` (empty body) | 10s (default) | `{"uri": string}` |
| `call_auth` | `auth` | JSON-stringified `auth_info` (`user`/`password` at minimum) | 5s | `{"allow": bool, "username"?: string, "display_name"?: string}` |
| `call_djon` | `djon` | `{"user": string}` | 10s (default) | `bool` |
| `call_djoff` | `djoff` | `{"user": string}` | 10s (default) | `bool` |
| `call_feedback` | `feedback` | `{song_id?, media_id?, playlist_id?, sq_id?, artist?, title?}` | 10s (default) | `bool` |
| `call_cp` | `cp` | `{"uri": string}` | caller-supplied (dynamic per SPEC.md D.6) | `{"uri": string, "isTemp": bool}` |
| `call_savecache` | `savecache` | `{"cache_key": string, "data": <object>}` | 5s | `bool` |

### What's actually wired up vs. spec'd-but-inert

- **Live in Phase 3**: `nextsong` (via `src/autodj.rs`'s retry-aware `fetch_next_track`) and `cp`
  (via `src/media.rs`, called unconditionally per track -- see its doc comment for the
  local-vs-api scope simplification) and `feedback` (via `src/feedback.rs`'s dedup tracker,
  called from `pipeline.rs` at the start of each crossfade transition).
- **Live in Phase 4**: `auth` and `djon`/`djoff` (via `src/harbor.rs`'s connection handler --
  see "Harbor (live-DJ input)" below).
- **Still spec'd-but-inert**: `savecache` remains a real, spec-correct function on
  `CallbackClient` that nothing calls yet -- it needs autocue branch 2 (deferred, see below) to
  have a trigger point.

## Harbor (live-DJ input)

`src/harbor.rs` implements SPEC.md B.4 (`writeHarborConfiguration`) and C.4 (DJ
authentication/connect/disconnect sequencing): a TCP listener that accepts a source-client
connection, authenticates it against PHP, decodes its live audio incrementally, and gives it
playback priority over AutoDJ.

**Protocol scope**: only the Icecast2/HTTP-style source handshake -- a `SOURCE <mount> ICE/1.0`
or `PUT <mount> HTTP/1.0` request line, HTTP-style headers terminated by a blank line, credentials
via `Authorization: Basic <base64(user:pass)>`. This is what modern source clients (BUTT, Mixxx,
ffmpeg's icecast muxer) send. **Legacy Shoutcast1/2 framing (non-HTTP-style) is not implemented.**
`src/harbor.rs::parse_handshake` is the pure, unit-tested parser for this handshake.

**Gating**: the listener does not bind at all if `harbor.enabled = false` (or the `[harbor]`
section is absent), matching SPEC.md B.4's `!station->enable_streamers` no-op.

**Decode**: streaming/chunked via `symphonia`, wrapping the live TCP connection directly as a
`symphonia::core::io::ReadOnlySource` (an unseekable `MediaSource` -- confirmed via
`symphonia-core`'s own source, which provides this wrapper specifically for non-seekable streams
like a live network connection). This runs on a dedicated blocking OS thread
(`tokio::task::spawn_blocking`), since `symphonia`'s reads are synchronous. Each decoded chunk is
resampled/remixed to the pipeline's fixed 44100Hz-stereo format via a new persistent
`decode::StreamResampler` (shares the same `rubato`-based approach as `decode.rs`'s full-buffer
path, but keeps its resampler instance alive across many small chunks instead of one whole-file
batch loop). This is a separate code path from `decode.rs`'s existing full-buffer AutoDJ-file
decode, which is unmodified.

**Readiness / priority**: a connection becomes the active playback source once it is connected +
authenticated + has produced at least one decoded chunk of audio (`harbor::LiveState::is_ready`).
`autodj::fetch_next_track` checks this in the correct priority slot: `interrupting_requests` >
live > `requests` > AutoDJ (SPEC.md C.8). The moment live becomes ready, the currently-playing
AutoDJ/queued track is force-skipped (SPEC.md B.4 #3's `check_live()`, reusing the existing
`/skip` mechanism from `src/control.rs` rather than a second "abandon current track" path).

**`djon`/`djoff` sequencing** follows SPEC.md C.4 exactly: `djon` fires asynchronously
(fire-and-forget) right after a successful handshake, not blocking the start of decode. On
disconnect (voluntary or forced via `/streamer/disconnect`), readiness flips false
immediately/synchronously; `djoff` then fires asynchronously, and the DJ's identity
(`live_dj()`/`live_dj_name()`) stays populated until that call completes.

**Crossfade "to-live" special case** (SPEC.md C.5 point 1): implemented as a new dispatch branch
in `src/crossfade.rs`, gated by `CrossfadeParams::to_live`. When live has just become ready, the
station's configured crossfade mode/dB-analysis is ignored entirely: the outgoing track fades out
completely over `default_fade`, *then* the live audio fades in over `default_fade` -- sequential,
not overlapped. `src/pipeline.rs` sets this flag only for the single AutoDJ-to-live transition;
live-to-live continuation uses no crossfade/windowing at all (the real Liquidsoap `live` source is
never itself passed through `cross()`), and live-to-AutoDJ (the DJ disconnecting) uses the
station's normal configured dispatch -- SPEC.md C.5 explicitly notes there is no special
"returning from live" branch, only "going to live" is special. See `pipeline.rs`'s module doc for
the full breakdown of all three transition shapes.

**Explicitly out of scope / deferred**:
- Legacy Shoutcast1/2 handshake protocol.
- Mid-stream ICY metadata updates from the source client (some encoders send periodic metadata
  updates during the connection) -- not parsed; a live session's `PreparedTrack` metadata is
  always empty, which also means `feedback` is naturally suppressed for live audio via
  `FeedbackDedup`'s existing "no reportable metadata" guard. This coincidentally matches SPEC.md
  B.4 #3's own note that `is_live` metadata attachment (`insert_missing`) is commented out
  ("Temporarily disabled for testing") in the real Liquidsoap config generation -- a verified
  no-op this engine also doesn't implement.
- Recording the live stream to disk (SPEC.md B.4 point 4, `record_streams`).

Live-DJ audio flows through the exact same crossfade/mixing pipeline as AutoDJ tracks (see
`pipeline.rs`), so it's already included in whatever `src/output.rs` (below) pushes out to Icecast
— there's no separate "live output path".

## Network output (Icecast/Shoutcast source-client push, Phase 5)

`src/output.rs` implements the actual network output path: encodes the pipeline's mixed PCM audio
(the same stream `pipeline.rs` writes to its local file sink) and pushes it, as an Icecast2
source-client, to the station's own local Icecast frontend (`[icecast_output]` + one connection per
`[[mounts]]` entry) and to each configured `[[remotes]]` relay target — independently. This is the
outbound mirror of `harbor.rs`'s inbound handshake: `harbor.rs` *accepts* a `SOURCE`/`PUT` request
and parses it; `output.rs` *builds and sends* one, then reads the target's response — the two
halves of the same protocol, cross-checked against each other in `output.rs`'s unit tests (the
outbound request `output.rs::build_source_request` produces is fed straight into
`harbor.rs::parse_handshake` and asserted to parse correctly).

**Protocol scope**: Icecast2 source-client only — `SOURCE <mount> ICE/1.0` request line,
`Authorization: Basic <base64(user:pass)>` + `Content-Type` + `ice-public` headers, then the
target's `200`-style response, then the encoded audio stream as the request body. No legacy
Shoutcast1/2 framing (matching `harbor.rs`'s own inbound scope limitation).

**Encoding**: shells out to an `ffmpeg` subprocess per output target — raw interleaved `f32` PCM at
the pipeline's fixed 44100Hz/stereo format piped in via stdin (`-f f32le -ar 44100 -ac 2 -i
pipe:0`), encoded container bytes read back out via stdout. Format/bitrate come from that target's
`format`/`bitrate` config fields. Container per format: `mp3`→`mp3`, `aac`→`adts` (raw ADTS
framing, the conventional streaming container for AAC), `ogg`→`ogg`, `opus`→`ogg` (Opus is
conventionally carried in an Ogg container for source-client streaming), `flac`→`flac`. AAC uses
ffmpeg's built-in `aac` encoder rather than `libfdk_aac` — `libfdk_aac`'s license keeps it out of
most stock ffmpeg builds, so the encoder that's actually likely to be present is used instead; a
documented judgment call, not an oversight.

**No `share_encoders`** (`engine/SPEC.md` B.6): every mount and every remote gets its own
independent `ffmpeg` process and its own independent Icecast connection, even if two outputs have
identical format/bitrate. A deliberate, documented scope simplification (matching other
already-established simplifications elsewhere in this engine), not a missing optimization.

**Resilience**: each output target (`output::run_output_target`) is its own independent,
infinitely-retrying `tokio::spawn` task with a fixed 5-second reconnect backoff on any failure
(connect failure, handshake rejection, ffmpeg spawn failure, or the connection simply dropping).
There's no SPEC.md/Liquidsoap precedent for this behavior (it's new engine-side output, not a
documented Liquidsoap re-implementation), so a fixed backoff is used rather than anything more
elaborate. One target being unreachable never affects any other target, nor the AutoDJ/harbor
pipeline itself — the pipeline keeps decoding/mixing/crossfading and broadcasting to its internal
fan-out tap regardless of whether any (or all) network outputs are currently connected.

**Fan-out mechanism**: `pipeline.rs`'s `OutputSink` broadcasts every chunk of mixed PCM it produces
over a `tokio::sync::broadcast` channel (`main.rs` constructs it and hands a `Sender` to
`Pipeline::new` and a clone to each spawned output task). A lagging output task's receiver just
skips forward to newer audio (logged) rather than blocking the pipeline or any other output task.

**Explicitly out of scope / deferred** (see "Known follow-ups" below for the complete list):
legacy Shoutcast/RSAS protocols for `[[remotes]]` (only `protocol = "icecast"` is handled — any
other value is logged and that single remote is skipped, not a fatal config error),
`share_encoders`, and mid-stream ICY metadata updates pushed over an already-open source
connection. HLS output (`engine/SPEC.md` B.8) was also originally deferred here but is now
implemented -- see `hls.rs`.

## Files

Phase 2:
- `src/config.rs` — `EngineConfig` and friends; `load_config` (from a file path) and
  `parse_config` (from an in-memory string, used by both file loading and `--check-config`).
  Phase 3 additions: `station.replaygain_enabled`, `paths.fallback_file_path`,
  `paths.pipeline_output_path`, and the new `[crossfade]` section (`CrossfadeConfig`).
- `src/callbacks.rs` — `CallbackClient` with all seven callback functions (unchanged in Phase 3).
- `src/server.rs` — `AppState` and `build_router`, the control API. Phase 3: `AppState` gained
  `queues: Arc<TrackQueues>`; the two queue handlers now mutate/read it for real. This phase:
  `AppState` also gained `control: Arc<ControlSignals>`; `skip_handler`/`metadata_handler` now
  dispatch through it instead of just logging.
- `src/main.rs` — argument parsing (`--version`, `--check-config -`, `--config <path>`, Phase 3's
  `--crossfade-test`), logging setup, and the concurrent server + pipeline lifecycle with
  signal-based shutdown.

Phase 3 (new):
- `src/annotate.rs` — parses `annotate:key="val",...:path` URIs (the inverse of PHP's
  `ConfigWriter::annotateValue`/`annotateArray`, SPEC.md B.15) into a `HashMap` + bare path.
  Unit-tested.
- `src/media.rs` — resolves a bare path/URI to a local file via `cp` (SPEC.md D.6), and cleans up
  `isTemp` files after use.
- `src/decode.rs` — full in-memory decode (`symphonia`) + resample/remix to 44100Hz stereo
  (`rubato`); also reads a `REPLAYGAIN_TRACK_GAIN`-style tag off the file if present.
- `src/prepare.rs` — applies branch-1 autocue trim, `liq_amplify`, and (if enabled) replaygain to
  a decoded track; extracts `feedback`-relevant metadata from annotations. `TrackMetadata::apply_overrides`
  (this phase) merges a `/metadata` control-API override onto it. Unit-tested.
- `src/queue.rs` — the `requests`/`interrupting_requests` priority queues. Unit-tested.
- `src/autodj.rs` — ties queues + `nextsong` + `cp` + decode + prepare together with SPEC.md
  C.3's exact 10s retry delay and the fallback-file/silence safety net.
- `src/crossfade.rs` — `cross.smart`'s five-branch dB dispatch (loudness via `ebur128`), plus
  `normal`/`disabled` modes, plus the actual sample-buffer mixing math. Unit-tested (branch
  selection + mix math, no audio files needed).
- `src/feedback.rs` — `last_title`/`last_artist` dedup + jingle/error-file suppression for the
  `feedback` callback.
- `src/pipeline.rs` — the live playback loop tying everything above together; see its module doc
  for the buffer-position-driven lookahead/decode timing model and (Phase 6) the separate
  wall-clock output-emission pacing layered on top of it. Phase 3: polls `ControlSignals` once
  per loop iteration for `/skip`/`/metadata` requests. This phase: split into
  `advance_from_autodj`/`advance_from_live` to handle the three live-transition shapes
  (AutoDJ->live, live->live, live->AutoDJ) -- see its module doc.

Phase 3.5 (control-API follow-up):
- `src/control.rs` — `ControlSignals`, the shared `/skip` + `/metadata` signal handle between
  `server.rs`'s axum handlers and `pipeline.rs`'s loop. A non-blocking poll, not a blocking wait
  -- see its module doc for why. Unit-tested.

This phase (new):
- `src/harbor.rs` — the live-DJ harbor TCP listener: handshake parsing (`parse_handshake`, unit
  -tested), `LiveState` (shared readiness/session/chunk-channel state), the connection handler
  (`auth`/`djon`/`djoff` sequencing), and the streaming decode loop
  (`run_live_decode`, blocking, run via `spawn_blocking`). See "Harbor (live-DJ input)" above.
- `src/config.rs` — new `HarborConfig`/`EngineConfig.harbor` (the `[harbor]` TOML section).
- `src/decode.rs` — `to_stereo_channel_planes`/`interleave_stereo`/`append_interleaved` made
  `pub(crate)` (shared with `harbor.rs`); new `StreamResampler` (incremental counterpart to the
  existing whole-file `resample_channels`, used only by `harbor.rs`). `decode_to_pcm`'s own
  full-buffer path is unchanged.
- `src/prepare.rs` — new `PreparedTrack::is_live` field and `prepare_live_chunk` constructor.
- `src/crossfade.rs` — new `CrossfadeParams::to_live` field and the to-live dispatch branch
  (`to_live_transition`). Unit-tested (branch dispatch + fade-shape assertions, same style as the
  existing `select_smart_branch` tests).
- `src/queue.rs` — `pop_next` replaced by `pop_interrupting`/`pop_requests` (called separately by
  `autodj::fetch_next_track` with a live-readiness check in between, since live now sits between
  them in priority order).
- `src/autodj.rs` — `fetch_next_track` takes a new `live: Option<&LiveState>` parameter and checks
  it in the `interrupting_requests` > live > `requests` > AutoDJ priority order (SPEC.md C.8).
- `src/server.rs` — `AppState` gained `live: Arc<LiveState>`; `streamer_disconnect_handler` is now
  real (calls `LiveState::force_disconnect`).
- `src/main.rs` — constructs `LiveState`, threads it into `AppState`/`Pipeline::new`, and spawns
  the harbor listener task alongside the control API and pipeline tasks.

Phase 5 (new):
- `src/output.rs` — the network output path: `OutputFormat` (format→ffmpeg-codec/container/
  content-type mapping, unit-tested), `ffmpeg_args`/`build_ffmpeg_command` (ffmpeg CLI argument
  construction, unit-tested per format), `IcecastTarget`/`build_source_request`
  (outbound handshake construction, unit-tested and cross-checked against `harbor.rs`'s own
  parser), `connect_and_handshake` + `run_output_once`/`run_output_target` (the actual
  connect/encode/stream/retry loop), and `build_targets` (config → target list, with
  unrecognized-format/unsupported-protocol entries logged and skipped rather than failing config
  load). Includes a real-ffmpeg integration test that skips (not fails) if `ffmpeg` isn't on `PATH`.
- `src/config.rs` — new `IcecastOutputConfig`/`MountConfig`/`RemoteConfig` and
  `EngineConfig.icecast_output`/`mounts`/`remotes` (the `[icecast_output]`/`[[mounts]]`/
  `[[remotes]]` TOML sections), all `#[serde(default)]` so configs without them still parse.
- `src/pipeline.rs` — `OutputSink` (new) wraps the Phase 3 local-file sink together with a new
  `tokio::sync::broadcast` fan-out tap; every chunk of mixed PCM the pipeline produces now goes to
  both. `Pipeline::new` takes a new `audio_tap: broadcast::Sender<Arc<Vec<f32>>>` parameter.
  `advance_from_live`/`advance_from_autodj` now take `&mut OutputSink` instead of
  `&mut Option<File>`. No change to the pipeline's actual decode/crossfade/timing logic.
- `src/main.rs` — constructs the `broadcast` channel, spawns one `output::run_output_target` task
  per target returned by `output::build_targets(&cfg)`, and threads the channel's `Sender` into
  `Pipeline::new`.

Phase 6 (new):
- `src/pipeline.rs` — real-time wall-clock pacing for output *emission*, layered on top of the
  existing buffer-position-driven lookahead/decode logic (which is unchanged and stays eager).
  New `StreamClock` (one shared `start: tokio::time::Instant` + running `frames_emitted: u64` for
  the whole `Pipeline::run()` loop lifetime, continuous across track and live/AutoDJ boundaries)
  and the pure, unit-tested `pacing_sleep_duration` function it wraps. Every call site that writes
  to `OutputSink` (`advance_from_autodj`'s body write and crossfade-transition write,
  `advance_from_live`'s straight-through write and crossfade-transition write) now goes through
  `paced_write_frame_range`/`paced_write_all`, which sleep (via `tokio::time::sleep`, never
  `std::thread::sleep`) just long enough to keep total frames emitted in step with wall-clock time
  before writing, then commit the chunk to the shared clock. Falling behind wall clock (e.g. a slow
  decode/mix) naturally results in *not* sleeping on the next write rather than any "catch up by
  skipping audio" logic. This sleep only blocks `Pipeline::run()`'s own spawned task -- the
  control-API server and harbor TCP listener are separate spawned tasks (see `main.rs`) and are
  unaffected. Live-harbor chunks use the exact same pacing mechanism as AutoDJ chunks (no
  live/AutoDJ special-casing): since live audio already arrives at roughly real-time pace, pacing
  is a near-no-op for it in practice.

## Known follow-ups / explicitly out of scope

- **Autocue branch 2** (on-the-fly loudness-based cue computation + the `savecache` round trip,
  SPEC.md C.10) — only branch 1 (annotation-supplied cue points) and branch 3 (no autocue data)
  are implemented.
- **Per-station crossfade threshold config from PHP** — `[crossfade]` and
  `station.replaygain_enabled` are real, working TOML fields, but nothing on the PHP side
  (`StreamEngine::getCurrentConfiguration()`) populates them yet; they default to SPEC.md's own
  stated defaults until that wiring exists.
- **Streaming/chunked decode for the AutoDJ path** — `decode.rs`'s `decode_to_pcm` (full-file
  AutoDJ tracks) still fully materializes each track in memory; fine for this phase, an
  optimization opportunity later. (The live-DJ harbor path added this phase, `harbor.rs`, *is*
  streaming/chunked, since a live connection has no known length/EOF until the DJ disconnects.)
- ~~**Real-time output pacing**~~ — implemented in Phase 6: `pipeline.rs`'s `StreamClock` paces
  every write to `OutputSink` (both the local file sink and the Phase 5 network fan-out tap) to
  real wall-clock time, independent of the eager/unpaced decode+crossfade lookahead. See
  `pipeline.rs`'s module doc and the Phase 6 entry above for the design.
- ~~**HLS output**~~ (`engine/SPEC.md` B.8) — implemented post-cutover: `src/hls.rs`, one ffmpeg
  segmenter per `StationHlsStream` writing directly to `station->getRadioHlsDir()`, plus a
  multi-bitrate master `live.m3u8`. See the Phase 5 section above for the design summary. Always
  encoded as AAC-LC regardless of a station's nominally-configured `HlsStreamProfiles` value (HE-AAC
  profiles would need `libfdk_aac`, which this engine deliberately avoids everywhere — see
  `output.rs`'s module doc for the same reasoning applied to Icecast/relay AAC output).
- **Legacy Shoutcast1/2 source protocol**, both directions — only the Icecast2/HTTP-style handshake
  (`SOURCE`/`PUT` + HTTP headers + `Authorization: Basic`) is implemented, for both `harbor.rs`'s
  inbound accept path and `output.rs`'s outbound push path. `[[remotes]]` entries with
  `protocol` other than `"icecast"` are logged and skipped, not attempted.
- **Mid-stream ICY metadata updates** — neither parsed from an inbound live-DJ source client
  (`harbor.rs`) nor pushed to an outbound Icecast connection mid-stream (`output.rs`); see "Harbor
  (live-DJ input)" above for why the inbound side is a safe, verified no-op. The outbound side is a
  possible future enhancement, not implemented.
- **`share_encoders`** (`engine/SPEC.md` B.6, single-shared-encoder-instance-per-format
  optimization across multiple outputs) — not implemented; every mount/remote gets its own
  independent `ffmpeg` process and connection, a deliberate documented scope simplification.
- **Recording the live stream to disk** (SPEC.md B.4 point 4, `record_streams`) — not implemented.
- **`harbor.buffer_secs`/`max_buffer_secs`** — parsed and logged, not currently acted on (this
  engine's live decode path is unbuffered/streaming by construction).
