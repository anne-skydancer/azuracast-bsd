# azuracast-engine

A Rust replacement for AzuraCast's Liquidsoap (OCaml) streaming engine, built in phases. See
`SPEC.md` in this directory for the full behavioral specification of the Liquidsoap integration
this engine is replacing.

**This is Phase 3: real AutoDJ decode/crossfade playback.** Phase 2 built the skeleton (process
lifecycle, control API, callback client). Phase 3 adds the actual audio pipeline: annotation-string
parsing, `media:`/`cp` resolution, full decode (MP3/AAC/OGG-Vorbis/FLAC/WAV via `symphonia`,
resampled to 44100Hz stereo via `rubato`), branch-1 autocue trim + `liq_amplify` + replaygain,
the two priority request queues (now wired for real to `/queue/{queue}/push` and
`/queue/{queue}/empty`), AutoDJ polling with SPEC.md's exact 10s retry delay, `cross.smart`'s
five-branch dB-aware crossfade dispatch (loudness measured via `ebur128`) plus `normal`/`disabled`
modes, `feedback` dedup + jingle/error-file suppression, and a fallback/error-file safety net.
Output goes to a **local file sink only** — no network broadcast (Icecast/Shoutcast/HLS) yet, and
no live-DJ harbor input yet; both are later phases. See the bottom of this file for what's
deliberately still out of scope.

## Building

```sh
cargo build --release
```

The binary is produced at `target/release/azuracast-engine` (`azuracast-engine.exe` on Windows).

Dependencies (beyond Phase 2's `tokio`/`axum`/`reqwest`/`serde`/`toml`/`tracing`): `symphonia`
(pure-Rust decode: MP3, AAC/ADTS+MP4, OGG/Vorbis, FLAC, WAV/PCM), `rubato` (sample-rate
conversion), `ebur128` (loudness measurement for crossfade dB-level comparisons), `hound` (WAV
read/write, used only by the `--crossfade-test` CLI path).

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
```

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
| POST | `/skip` | — | **Real.** Signals `pipeline.rs`'s loop (via the shared `ControlSignals` handle in `src/control.rs`) to abandon the rest of the currently-playing track's body on its next loop check and jump straight to the crossfade into the next track, as if the body had naturally ended there -- SPEC.md C.9's `add_skip_command`/`source.skip(s)`. `200 {"ok": true}` is returned immediately (fire-and-forget); the pipeline has no real-time output pacing yet (see `pipeline.rs`'s module doc), so "skip" here means the next produced output chunk jumps to the crossfade, not an instantaneous, audible real-time cut -- that distinction is deferred to Phase 5's real-time-paced output sink. |
| POST | `/queue/{queue}/push` | `{"uri": "<string>"}` | **Real in Phase 3.** Pushes onto the actual in-memory `requests`/`interrupting_requests` queue (`src/queue.rs`), which `pipeline.rs` pops from in priority order ahead of AutoDJ. Unrecognized `{queue}` names get `400 {"ok": false, "error": "..."}` instead of the old silent-log behavior. |
| GET | `/queue/{queue}/empty` | — | **Real in Phase 3.** Reports the real queue's emptiness; unrecognized queue names report `true` (nothing to report). |
| POST | `/metadata` | JSON object, string values (`HashMap<String, String>`) | **Real.** Stages the given map (via the same `ControlSignals` handle) as an override to be merged onto the currently-playing track's metadata and re-pushed through the same `FeedbackDedup::maybe_send` dedup-and-push path every other metadata change uses -- SPEC.md C.9's `add_custom_metadata_command`/`insert_metadata`. `200 {"ok": true}` is returned immediately; the merge/re-push happens on the pipeline's next loop check, same "not instantaneous" caveat as `/skip`. Only the six fields `feedback`/C.6 forwards (`title`, `artist`, `song_id`, `media_id`, `sq_id`, `playlist_id`) are recognized -- other keys are accepted but ignored. |
| POST | `/streamer/disconnect` | — | Logs "streamer disconnect requested". `200 {"ok": true}`. Still a no-op (Phase 4, live-DJ harbor). |

`/skip`, `/metadata`, and both queue routes have real effects as of this phase -- only
`/streamer/disconnect` remains a log-and-return stub, pending Phase 4's live-DJ harbor.

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
- **Still spec'd-but-inert**: `auth`, `djon`, `djoff`, `savecache` remain real, spec-correct
  functions on `CallbackClient` that nothing calls yet -- they need live-DJ harbor input
  (Phase 4) or autocue branch 2 (deferred, see below) to have a trigger point.

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
  for the buffer-position-driven timing model (no real-time output device exists yet). This
  phase: polls `ControlSignals` once per loop iteration for `/skip`/`/metadata` requests.

This phase (new):
- `src/control.rs` — `ControlSignals`, the shared `/skip` + `/metadata` signal handle between
  `server.rs`'s axum handlers and `pipeline.rs`'s loop. A non-blocking poll, not a blocking wait
  -- see its module doc for why. Unit-tested.

## Known follow-ups / explicitly out of scope for Phase 3

- **Autocue branch 2** (on-the-fly loudness-based cue computation + the `savecache` round trip,
  SPEC.md C.10) — only branch 1 (annotation-supplied cue points) and branch 3 (no autocue data)
  are implemented.
- **Per-station crossfade threshold config from PHP** — `[crossfade]` and
  `station.replaygain_enabled` are real, working TOML fields, but nothing on the PHP side
  (`StreamEngine::getCurrentConfiguration()`) populates them yet; they default to SPEC.md's own
  stated defaults until that wiring exists.
- **Streaming/chunked decode** — `decode.rs` fully materializes each track in memory; fine for
  this phase, an optimization opportunity later.
- **Real-time output pacing / Icecast/Shoutcast/HLS output** — Phase 5. The live pipeline writes
  to a local raw-PCM file sink (or nowhere, if unconfigured) as fast as it can decode/mix, not
  paced to wall-clock playback speed.
- **Live-DJ harbor input/merging, and the crossfade "to-live" special case (SPEC.md C.5 point
  1)** — Phase 4.
- **`/streamer/disconnect` control-API route** — still a log-and-return stub; needs the Phase 4
  live-DJ harbor to have something real to act on. (`/skip` and `/metadata` are now wired into
  `pipeline.rs` via `src/control.rs`'s `ControlSignals`.)
