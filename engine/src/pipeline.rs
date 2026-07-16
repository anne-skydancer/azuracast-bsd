//! Live playback pipeline orchestration: ties together `queue.rs`,
//! `autodj.rs`, `decode.rs`, `prepare.rs`, `crossfade.rs`, `feedback.rs`,
//! and (Phase 4) `harbor.rs` into the actual running engine loop that
//! replaces Phase 2's demo `nextsong_loop`.
//!
//! **Timing model:** the *lookahead/decode* side of this loop is still
//! driven by *buffer position* (how many frames of the current track have
//! been consumed) rather than a wall clock -- the AutoDJ lookahead
//! requirement ("fetch when `default_fade` + a small buffer remains") is
//! satisfied structurally instead of temporally: the next track is always
//! fully resolved, decoded, and prepared *before* the crossfade transition
//! into it is computed. This deliberately keeps decode/crossfade running
//! eager and unpaced -- only the *emission* of finished chunks to the output
//! sinks (`OutputSink`'s file write + Phase 5's broadcast tap) is paced to
//! real wall-clock time (Phase 6), via a single shared `StreamClock` (start
//! instant + running total frames emitted) that every write call site in
//! this file consults before writing: `advance_from_autodj`'s body and
//! crossfade-transition writes, and `advance_from_live`'s straight-through
//! and crossfade-transition writes. The clock is continuous across track
//! and live/AutoDJ boundaries (never reset per-track), so playback timing
//! doesn't jump at transitions. If a chunk's production (decode/mix) falls
//! behind wall clock, pacing naturally falls through without sleeping
//! rather than trying to "catch up" by skipping audio -- see
//! `StreamClock::sleep_before_chunk`'s doc.
//!
//! **Network output (Phase 5):** every chunk of mixed PCM this loop
//! produces is now also broadcast (via `OutputSink`'s `tap`) to zero or more
//! independent encode+push tasks (`output.rs`), one per configured
//! `[[mounts]]`/`[[remotes]]` entry -- see `output.rs`'s module doc for the
//! full scope. This is purely additive: the local raw-PCM file sink from
//! Phase 3 is untouched and still available for local testing/debugging
//! alongside the new real network outputs.
//!
//! **Live-DJ harbor integration (Phase 4):** live audio chunks arrive as
//! `PreparedTrack`s just like AutoDJ tracks (see `harbor.rs`'s module doc),
//! but three transition shapes need distinct handling, per SPEC.md C.5's
//! explicit going-to-live/coming-from-live asymmetry:
//! - **AutoDJ -> live** (the moment `LiveState::poll_transition` reports
//!   the to-live edge): the AutoDJ track is force-skipped (reusing the
//!   existing `/skip` mechanism, per SPEC.md B.4 #3's `check_live()`), and
//!   the transition into the first live chunk uses `crossfade.rs`'s
//!   special to-live branch (`CrossfadeParams::to_live = true`), with both
//!   fade durations pinned to the station's plain `default_fade` --
//!   ignoring any per-track autocue fade override, matching SPEC.md C.5
//!   point 1's "ignores crossfade settings entirely".
//! - **live -> live** (the DJ is still connected, both `current` and
//!   `next` are live chunks): no crossfade/windowing at all -- the real
//!   Liquidsoap `live` source is never itself passed through `cross()`
//!   (SPEC.md B.4 #3's own comment), so consecutive chunks of the same
//!   continuous stream are just written straight through.
//! - **live -> AutoDJ** (the DJ disconnected): SPEC.md C.5 explicitly notes
//!   there is *no* special "returning from live" branch -- this uses the
//!   station's normal configured crossfade dispatch, exactly like an
//!   AutoDJ-to-AutoDJ transition.

use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::broadcast;

use crate::audio_processing::AudioProcessor;
use crate::autodj;
use crate::callbacks::CallbackClient;
use crate::config::EngineConfig;
use crate::control::ControlSignals;
use crate::crossfade::{self, CrossfadeMode, CrossfadeParams, CrossfadeThresholds};
use crate::decode::{PIPELINE_CHANNELS, PIPELINE_SAMPLE_RATE};
use crate::feedback::FeedbackDedup;
use crate::harbor::LiveState;
use crate::prepare::PreparedTrack;
use crate::queue::TrackQueues;

pub struct Pipeline {
    client: Arc<CallbackClient>,
    queues: Arc<TrackQueues>,
    /// Shared `/skip` + `/metadata` control-API signal handle -- see
    /// `control.rs`'s module doc. Polled (non-blockingly) once per loop
    /// iteration below.
    control: Arc<ControlSignals>,
    /// Phase 4: shared live-DJ harbor state (`harbor.rs`) -- chunk source,
    /// readiness, and the to-live transition edge.
    live: Arc<LiveState>,
    replaygain_enabled: bool,
    fallback_file_path: Option<String>,
    crossfade_mode: CrossfadeMode,
    thresholds: CrossfadeThresholds,
    default_fade_secs: f64,
    cross_window_secs: f64,
    output_path: Option<String>,
    feedback: FeedbackDedup,
    /// Phase 5: fan-out tap that every produced PCM chunk is broadcast to,
    /// independent of the local file sink -- see `output.rs`'s module doc.
    /// Cloned into `main.rs` so it can be handed to each independent
    /// mount/remote output task's `broadcast::Receiver`.
    audio_tap: broadcast::Sender<Arc<Vec<f32>>>,
    /// Post-cutover audio post-processing (`nrj`/`stereo_tool`/none) -- see
    /// `audio_processing.rs`. One instance for the whole pipeline lifetime
    /// (never reset per-track), since both `NrjProcessor`'s smoothed
    /// gain/envelope state and `StereoToolProcessor`'s subprocess pipe need
    /// continuity across track boundaries, exactly like `StreamClock`.
    audio_processor: AudioProcessor,
    /// Whether `audio_processor` applies to live-DJ chunks too, or only to
    /// AutoDJ-sourced audio (`StationBackendConfiguration::
    /// post_processing_include_live`). Every write inside
    /// `advance_from_live` is gated by this flag; every write inside
    /// `advance_from_autodj` is always processed (that branch's `current`
    /// is never live, by construction) -- see `audio_processing.rs`'s
    /// module doc for why per-branch (not per-write) is the chosen
    /// granularity.
    audio_include_live: bool,
}

impl Pipeline {
    pub fn new(
        client: Arc<CallbackClient>,
        queues: Arc<TrackQueues>,
        control: Arc<ControlSignals>,
        live: Arc<LiveState>,
        cfg: &EngineConfig,
        audio_tap: broadcast::Sender<Arc<Vec<f32>>>,
    ) -> Self {
        Self {
            client,
            queues,
            control,
            live,
            replaygain_enabled: cfg.station.replaygain_enabled,
            fallback_file_path: cfg.paths.fallback_file_path.clone(),
            crossfade_mode: cfg.crossfade.mode(),
            thresholds: cfg.crossfade.thresholds(),
            default_fade_secs: cfg.crossfade.fade_seconds,
            cross_window_secs: cfg.crossfade.fade_seconds.max(crossfade::DEFAULT_CROSS_SECONDS),
            output_path: cfg.paths.pipeline_output_path.clone(),
            feedback: FeedbackDedup::new(),
            audio_tap,
            audio_processor: AudioProcessor::from_config(&cfg.audio_processing),
            audio_include_live: cfg.audio_processing.include_live,
        }
    }

    /// Runs forever (until the process is torn down), matching the
    /// lifecycle of Phase 2's `nextsong_loop`.
    pub async fn run(mut self) {
        let file = self.output_path.as_deref().and_then(open_output_sink);
        if self.output_path.is_some() && file.is_none() {
            tracing::error!(
                "failed to open pipeline output sink; continuing without file output"
            );
        }
        let mut output = OutputSink {
            file,
            tap: self.audio_tap.clone(),
        };

        // Phase 6: real-time wall-clock pacing state for output emission --
        // see this file's module doc and `StreamClock`'s doc. One shared
        // clock for the whole loop lifetime (not reset per-track or at
        // live/AutoDJ transitions), so playback stays continuous/monotonic
        // across every boundary. Created BEFORE the first fetch so the
        // gapless fetch below can pump paced silence from t=0 -- the
        // startup resolve/decode gap is subject to the same Icecast
        // source-timeout as every track-boundary gap.
        let mut clock = StreamClock::new(tokio::time::Instant::now());

        let mut current = self.fetch_next_gapless(&mut output, &mut clock).await;
        self.feedback.maybe_send(&self.client, &current).await;
        let mut played_frames = 0usize;

        loop {
            // `/metadata` (SPEC.md C.9's `insert_metadata`): apply any
            // pending override onto the *currently playing* track, then run
            // it back through the exact same feedback-dedup-and-push path
            // (`FeedbackDedup::maybe_send`) used for every other metadata
            // change -- C.6's dedup logic applies here too, so an override
            // identical to what was already reported is correctly
            // deduped/skipped rather than double-pushed. Non-blocking poll
            // (see `control.rs`'s module doc), so this takes effect on this
            // loop iteration -- i.e. "next check", not instantaneously.
            if let Some(overrides) = self.control.take_metadata_override() {
                current.metadata.apply_overrides(&overrides);
                self.feedback.maybe_send(&self.client, &current).await;
            }

            // SPEC.md B.4 #3's `check_live()`: the moment live becomes the
            // active source, force whatever AutoDJ/queue track is currently
            // playing to end immediately -- reusing the existing `/skip`
            // mechanism (see this file's module doc) rather than a second
            // "abandon current track" code path. This is a one-shot edge
            // (see `LiveState::poll_transition`): it only fires once per
            // live session, not on every loop iteration while live plays.
            if self.live.poll_transition() {
                tracing::info!(
                    "live harbor became ready; forcing AutoDJ track to abandon its body"
                );
                self.control.request_skip();
            }

            if current.is_live {
                let (next_current, next_played_frames) = self
                    .advance_from_live(&mut output, &mut clock, current, played_frames)
                    .await;
                current = next_current;
                played_frames = next_played_frames;
                continue;
            }

            let (next_current, next_played_frames) = self
                .advance_from_autodj(&mut output, &mut clock, current, played_frames)
                .await;
            current = next_current;
            played_frames = next_played_frames;
        }
    }

    /// Fetches the next track on a detached task while pumping paced
    /// silence chunks into the output, so the source connection to Icecast
    /// never goes idle during the resolve/decode gap between tracks.
    /// Confirmed on a real install: with the fetch awaited inline (the
    /// previous behavior), Icecast's `source-timeout` (10s in AzuraCast's
    /// generated config) reaped the source during every full-buffer decode,
    /// so the mount vanished at each track boundary and listeners got
    /// connection resets.
    ///
    /// Running the fetch on its own task also CONFINES prepare/decode
    /// panics: they surface here as a join error (logged, then retried --
    /// which naturally advances to the following queue entry) instead of
    /// tearing down the whole pipeline task, which previously exited the
    /// entire engine (see the symphonia zero-frame panic that crash-looped
    /// a real install before it was guarded in decode.rs).
    /// Spawns the next-track resolve/decode/prepare on a detached task and
    /// returns its handle. Spawning EARLY -- at the point a track starts
    /// playing, not when its successor is needed -- is what makes track
    /// transitions gapless in practice: the (potentially slow -- full-file
    /// decode + sinc resample over NFS, tens of seconds observed on a real
    /// install) preparation runs concurrently with minutes of playback, so
    /// by the crossfade point the result is almost always already waiting.
    fn spawn_fetch_task(&self) -> tokio::task::JoinHandle<PreparedTrack> {
        let client = Arc::clone(&self.client);
        let queues = Arc::clone(&self.queues);
        let live = Arc::clone(&self.live);
        let replaygain = self.replaygain_enabled;
        let fallback = self.fallback_file_path.clone();

        tokio::spawn(async move {
            autodj::fetch_next_track(
                &client,
                &queues,
                Some(live.as_ref()),
                replaygain,
                fallback.as_deref(),
            )
            .await
        })
    }

    /// Awaits a previously-spawned fetch, pumping paced silence chunks
    /// into the output the whole time, so the source connection to Icecast
    /// never goes idle however long the preparation takes. Confirmed on a
    /// real install: with the fetch awaited inline and no silence-fill,
    /// Icecast's `source-timeout` (10s in AzuraCast's generated config)
    /// reaped the source during every between-track gap.
    ///
    /// Running the fetch on its own task also CONFINES prepare/decode
    /// panics: they surface here as a join error (logged, then retried --
    /// which naturally advances to the following queue entry) instead of
    /// tearing down the whole pipeline task, which previously exited the
    /// entire engine (see the symphonia zero-frame panic that crash-looped
    /// a real install before it was guarded in decode.rs).
    async fn await_fetch_gapless(
        &mut self,
        output: &mut OutputSink,
        clock: &mut StreamClock,
        mut handle: tokio::task::JoinHandle<PreparedTrack>,
    ) -> PreparedTrack {
        let wait_started = tokio::time::Instant::now();

        // 100ms of stereo silence per pump: small enough to hand control
        // back quickly once the fetch completes, large enough to be cheap.
        let silence =
            vec![0.0f32; (PIPELINE_SAMPLE_RATE as usize / 10) * PIPELINE_CHANNELS as usize];

        loop {
            tokio::select! {
                res = &mut handle => match res {
                    Ok(track) => {
                        let waited = wait_started.elapsed();
                        if waited > Duration::from_millis(250) {
                            tracing::info!(
                                "next track was not ready at the transition point; \
                                 filled {waited:?} of dead air with silence"
                            );
                        }
                        return track;
                    }
                    Err(e) => {
                        tracing::error!(
                            "next-track fetch task panicked ({e}); retrying with the following queue entry"
                        );
                        handle = self.spawn_fetch_task();
                    }
                },
                // `should_process = false`: gap silence skips the audio
                // post-processor -- nothing to normalize in zeroes.
                () = paced_write_all(output, clock, &mut self.audio_processor, false, &silence) => {}
            }
        }
    }

    /// Spawn-and-await in one step, for the call sites with nothing useful
    /// to overlap the preparation with (pipeline startup, live-DJ chunk
    /// continuation).
    async fn fetch_next_gapless(
        &mut self,
        output: &mut OutputSink,
        clock: &mut StreamClock,
    ) -> PreparedTrack {
        let handle = self.spawn_fetch_task();
        self.await_fetch_gapless(output, clock, handle).await
    }

    /// One loop iteration when `current` is a live-DJ harbor chunk. Handles
    /// both live-to-live continuation (no crossfade at all) and
    /// live-to-AutoDJ (the DJ just disconnected -- normal crossfade
    /// dispatch, no special "returning from live" branch, per SPEC.md C.5).
    /// See this file's module doc for the rationale.
    async fn advance_from_live(
        &mut self,
        output: &mut OutputSink,
        clock: &mut StreamClock,
        current: PreparedTrack,
        played_frames: usize,
    ) -> (PreparedTrack, usize) {
        // SPEC.md's `radio skip` only targets the non-live AutoDJ chain
        // (`source.skip(s)` on `radio_without_live`), never `live` itself --
        // consume and ignore any pending request so it doesn't leak into a
        // later AutoDJ track's handling.
        if self.control.take_skip() {
            tracing::debug!(
                "skip requested while live is active; ignoring (skip only applies to AutoDJ)"
            );
        }

        let total_frames = current.decoded.frames();

        let next = self.fetch_next_gapless(output, clock).await;
        self.feedback.maybe_send(&self.client, &next).await;

        if next.is_live {
            // Continuing the same live stream: straight-through, no
            // fade/window at all -- the real `live` source is never itself
            // passed through `cross()` (SPEC.md B.4 #3's own comment).
            paced_write_frame_range(
                output,
                clock,
                &mut self.audio_processor,
                self.audio_include_live,
                &current.decoded.samples,
                played_frames,
                total_frames,
            )
            .await;
            return (next, 0);
        }

        // Live just ended; falling back to AutoDJ/queues uses the
        // station's *normal* configured crossfade dispatch -- SPEC.md C.5
        // explicitly notes there is no special "returning from live"
        // branch, only "going to live" is special.
        let window_frames = (self.cross_window_secs * PIPELINE_SAMPLE_RATE as f64).round() as usize;
        let tail_start_frames = total_frames.saturating_sub(window_frames).max(played_frames);
        paced_write_frame_range(
            output,
            clock,
            &mut self.audio_processor,
            self.audio_include_live,
            &current.decoded.samples,
            played_frames,
            tail_start_frames,
        )
        .await;

        let old_tail = &current.decoded.samples[tail_start_frames * 2..total_frames * 2];
        let head_frames = window_frames.min(next.decoded.frames());
        let new_head = &next.decoded.samples[..head_frames * 2];

        let params = CrossfadeParams {
            mode: self.crossfade_mode,
            fade_in_secs: next.fade_in_override.unwrap_or(self.default_fade_secs),
            // Live has no per-track autocue fade override.
            fade_out_secs: self.default_fade_secs,
            thresholds: self.thresholds,
            to_live: false,
        };
        let mixed = crossfade::mix_transition(old_tail, new_head, PIPELINE_SAMPLE_RATE, &params);
        // This transition mix still originates from the live-ending branch
        // (its "old" half is live audio), so it's gated the same as this
        // whole function's other writes -- see `audio_include_live`'s doc.
        paced_write_all(
            output,
            clock,
            &mut self.audio_processor,
            self.audio_include_live,
            &mixed,
        )
        .await;

        (next, head_frames)
    }

    /// One loop iteration when `current` is a plain AutoDJ/queued track --
    /// the original Phase 3 behavior, extended with the to-live special
    /// case (SPEC.md C.5 point 1) when `next` turns out to be the first
    /// live-DJ chunk.
    async fn advance_from_autodj(
        &mut self,
        output: &mut OutputSink,
        clock: &mut StreamClock,
        current: PreparedTrack,
        played_frames: usize,
    ) -> (PreparedTrack, usize) {
        // PREFETCH: start resolving/decoding the successor NOW, before
        // playing this track's body -- see `spawn_fetch_task`'s doc. By
        // the crossfade point below, minutes of playback have elapsed and
        // the result is (almost always) already waiting, so the audible
        // between-track gap collapses to ~zero. Confirmed on a real
        // install pre-prefetch: fetch-at-the-transition-point meant every
        // track boundary carried the full prep time (observed >60s on
        // large files over NFS) as on-air silence.
        let mut fetch_handle = self.spawn_fetch_task();

        let window_frames = (self.cross_window_secs * PIPELINE_SAMPLE_RATE as f64).round() as usize;

        let total_frames = current.decoded.frames();
        let mut body_end_frames = total_frames.saturating_sub(window_frames).max(played_frames);
        // Upper bound for the crossfade "old tail" slice below. Normally
        // this is just `total_frames` (the tail runs to the track's
        // natural end); a skip narrows it -- see below.
        let mut tail_end_frames = total_frames;

        // `/skip` (SPEC.md C.9's `add_skip_command` / `source.skip(s)`):
        // force-abandon whatever remains of `current`'s body and jump
        // straight to computing the crossfade into `next`, as if
        // `body_end_frames` had already been naturally reached. The
        // crossfade tail is pinned to one `window_frames`-sized chunk
        // starting *here* (not the untouched remainder of the track) --
        // otherwise the entire unplayed rest of the track would ride
        // along as an unfaded "tail" once the shorter of the two mix
        // inputs runs out (see `crossfade::mix_add_fade`), which would
        // defeat the point of skipping. Now that output emission is
        // wall-clock paced (Phase 6, see this file's module doc and
        // `StreamClock`), narrowing `body_end_frames` here has a genuine
        // real-time effect: fewer frames get queued through
        // `paced_write_frame_range` below, so the `StreamClock`'s target
        // for subsequent writes arrives sooner and a live listener
        // actually hears the track cut short into the crossfade, not just
        // "the next produced chunk happens to be the crossfade" as before
        // pacing existed. (Also used, unchanged, for the force-skip Phase
        // 4 issue on the to-live transition edge -- see `run`'s
        // `poll_transition` check above.)
        if self.control.take_skip() {
            tracing::info!("skip requested; abandoning remainder of current track body");
            body_end_frames = played_frames;
            tail_end_frames = (played_frames + window_frames).min(total_frames);
        }

        // This branch's `current` is never live (see `run`'s dispatch), so
        // every write here is always processed -- no `audio_include_live`
        // gating needed, unlike `advance_from_live`.
        paced_write_frame_range(
            output,
            clock,
            &mut self.audio_processor,
            true,
            &current.decoded.samples,
            played_frames,
            body_end_frames,
        )
        .await;

        // Collect the prefetched successor (spawned at the top of this
        // fn, likely long since finished). PRIORITY GUARD: the prefetch
        // resolved against the world as it was when this track STARTED --
        // if a live DJ has become ready since, SPEC.md C.8's priority
        // order says live wins, so the stale queue-track prefetch is
        // discarded and a fresh fetch (which will return the live chunk)
        // takes its place. Without this, the DJ's first audio would be
        // delayed behind one full prefetched AutoDJ track.
        if self.live.is_ready() {
            fetch_handle.abort();
            fetch_handle = self.spawn_fetch_task();
        }
        let next = self.await_fetch_gapless(output, clock, fetch_handle).await;

        // SPEC.md step 8: fire feedback at the point the new track's
        // audio starts becoming audible -- the start of the crossfade
        // transition into it -- rather than after the full crossfade
        // completes.
        self.feedback.maybe_send(&self.client, &next).await;

        let old_tail = &current.decoded.samples[body_end_frames * 2..tail_end_frames * 2];
        let head_frames = window_frames.min(next.decoded.frames());
        let new_head = &next.decoded.samples[..head_frames * 2];

        // SPEC.md C.5 dispatch branch 1: entering live ignores the
        // station's crossfade settings entirely and pins both fade
        // durations to the plain `default_fade` (not any per-track
        // autocue override).
        let entering_live = next.is_live;
        let (fade_out, fade_in, to_live) = if entering_live {
            (self.default_fade_secs, self.default_fade_secs, true)
        } else {
            (
                current.fade_out_override.unwrap_or(self.default_fade_secs),
                next.fade_in_override.unwrap_or(self.default_fade_secs),
                false,
            )
        };

        let params = CrossfadeParams {
            mode: self.crossfade_mode,
            fade_in_secs: fade_in,
            fade_out_secs: fade_out,
            thresholds: self.thresholds,
            to_live,
        };

        let mixed = crossfade::mix_transition(old_tail, new_head, PIPELINE_SAMPLE_RATE, &params);
        paced_write_all(output, clock, &mut self.audio_processor, true, &mixed).await;

        (next, head_frames)
    }
}

/// Pure computation of how long to sleep before emitting a chunk of
/// `chunk_frames` frames, given `elapsed` (wall-clock time since the stream
/// started) and `frames_emitted` (total frames already written). Returns
/// `Duration::ZERO` when the target time has already passed -- i.e. when
/// the pipeline has fallen behind wall clock (a slow decode/mix took longer
/// than the audio duration it produced). Per standard real-time-streaming
/// practice, falling behind is handled by simply not sleeping (falling
/// through immediately) rather than trying to "catch up" by skipping or
/// dropping audio -- `Duration::saturating_sub` gives exactly that
/// behavior for free, no special-casing needed.
///
/// Deliberately a free function of plain `Duration`/`u64`/`u32` values (no
/// `tokio::time::Instant`, no async, no I/O) so it's directly unit-testable
/// without a tokio runtime and without any real sleeping -- see this
/// module's tests.
fn pacing_sleep_duration(
    elapsed: Duration,
    frames_emitted: u64,
    chunk_frames: u64,
    sample_rate: u32,
) -> Duration {
    let target_secs = (frames_emitted + chunk_frames) as f64 / sample_rate as f64;
    let target = Duration::from_secs_f64(target_secs);
    target.saturating_sub(elapsed)
}

/// Real-time wall-clock pacing state (Phase 6) shared across the whole
/// `Pipeline::run()` loop lifetime -- one `start` instant recorded once at
/// pipeline startup and a running `frames_emitted` total, both continuous
/// across track and live/AutoDJ transitions (never reset per-track), so
/// paced output timing stays monotonic across every boundary. See this
/// file's module doc for the full design and `pacing_sleep_duration` for
/// the pure math this wraps.
struct StreamClock {
    start: tokio::time::Instant,
    frames_emitted: u64,
}

impl StreamClock {
    fn new(start: tokio::time::Instant) -> Self {
        Self {
            start,
            frames_emitted: 0,
        }
    }

    /// How long to sleep, as of `now`, before writing a chunk of
    /// `chunk_frames` frames at `sample_rate`. Does not mutate `self` --
    /// callers apply the sleep (if any) and then call `advance` themselves,
    /// keeping the "decide" and "commit" steps separate and each
    /// independently testable.
    fn sleep_before_chunk(
        &self,
        now: tokio::time::Instant,
        chunk_frames: u64,
        sample_rate: u32,
    ) -> Duration {
        let elapsed = now.saturating_duration_since(self.start);
        pacing_sleep_duration(elapsed, self.frames_emitted, chunk_frames, sample_rate)
    }

    /// Commits `chunk_frames` as emitted, after the corresponding sleep (if
    /// any) has completed and the chunk has been written.
    fn advance(&mut self, chunk_frames: u64) {
        self.frames_emitted += chunk_frames;
    }
}

/// Frames per emission sub-chunk (~100ms at the pipeline rate): small
/// enough that downstream consumers (Icecast, listeners, the HLS tap)
/// receive a genuinely continuous real-time stream, large enough that
/// per-chunk overhead (one tokio sleep + one broadcast send) is trivial.
const EMIT_CHUNK_FRAMES: usize = PIPELINE_SAMPLE_RATE as usize / 10;

/// Sleeps and writes `samples[from_frame..to_frame)` (in frames) to
/// `output` in real-time-paced ~100ms sub-chunks, applying `processor`
/// per sub-chunk when `should_process` is true and a processor is actually
/// configured. Every AutoDJ-path body write and live-path straight-through
/// write goes through this. A no-op (no sleep, no write, no clock advance,
/// no processing) when the range is empty.
///
/// Skips the processing step entirely (and the owned-copy allocation it
/// would require) whenever `processor` is `AudioProcessor::None` -- the
/// common case of no post-processing configured at all should cost nothing
/// beyond the `matches!` check, not an unconditional per-chunk copy.
async fn paced_write_frame_range(
    output: &mut OutputSink,
    clock: &mut StreamClock,
    processor: &mut AudioProcessor,
    should_process: bool,
    samples: &[f32],
    from_frame: usize,
    to_frame: usize,
) {
    // Emit in ~100ms sub-chunks, pacing EACH against the stream clock --
    // never the whole range at once. The previous single
    // pace-then-write of the entire range slept for the full range
    // duration (minutes, for a track body) while emitting NOTHING, then
    // dumped the whole body in one burst: downstream, Icecast's
    // source-timeout (10s) reaped the idle source connection long before
    // the burst arrived, so the mount only ever existed for an instant
    // at each track boundary and listeners could never connect
    // (confirmed on a real install -- the engine spent each "playing"
    // track parked in a single multi-minute kevent sleep). Real-time
    // streaming means a continuous trickle, not scheduled batch mail.
    let mut chunk_start = from_frame;
    while chunk_start < to_frame {
        let chunk_end = (chunk_start + EMIT_CHUNK_FRAMES).min(to_frame);
        pace(clock, (chunk_end - chunk_start) as u64).await;

        let slice = &samples[chunk_start * 2..chunk_end * 2];
        if should_process && !matches!(processor, AudioProcessor::None) {
            let mut chunk = slice.to_vec();
            processor.process(&mut chunk).await;
            output.write_all(&chunk);
        } else {
            output.write_all(slice);
        }
        chunk_start = chunk_end;
    }
}

/// Sleeps (if needed) to keep `clock` paced to real wall-clock time, applies
/// `processor` to the full interleaved `samples` buffer under the same
/// `should_process`/no-op-if-unconfigured rules as `paced_write_frame_range`,
/// then writes the result to `output` and commits it to `clock`. Every
/// crossfade-transition mix write (both the AutoDJ path and the live path's
/// live-ending transition) goes through this. A no-op when `samples` is
/// empty.
async fn paced_write_all(
    output: &mut OutputSink,
    clock: &mut StreamClock,
    processor: &mut AudioProcessor,
    should_process: bool,
    samples: &[f32],
) {
    if samples.is_empty() {
        return;
    }
    // Same ~100ms sub-chunked emission as `paced_write_frame_range` (see
    // its comment for why single-shot pace-then-write breaks streaming);
    // crossfade windows are seconds long, well past Icecast's patience.
    let frames = samples.len() / PIPELINE_CHANNELS as usize;
    paced_write_frame_range(output, clock, processor, should_process, samples, 0, frames).await;
}

/// Shared sleep-then-commit sequence used by both `paced_write_frame_range`
/// and `paced_write_all`. Only ever awaited from inside `Pipeline::run()`'s
/// own spawned task (see `main.rs`), so this sleep blocks nothing but that
/// one task -- the control-API server and harbor TCP listener run as
/// separate spawned tasks and are unaffected. `tokio::time::sleep` (not
/// `std::thread::sleep`) is what makes that true: it yields the task back
/// to the executor for the sleep duration instead of blocking a worker
/// thread.
async fn pace(clock: &mut StreamClock, chunk_frames: u64) {
    let sleep_dur = clock.sleep_before_chunk(tokio::time::Instant::now(), chunk_frames, PIPELINE_SAMPLE_RATE);
    if sleep_dur > Duration::ZERO {
        tokio::time::sleep(sleep_dur).await;
    }
    clock.advance(chunk_frames);
}

/// Combines the Phase 3 local-file debug sink with the Phase 5 network
/// output fan-out tap: every chunk of mixed PCM the pipeline produces goes
/// to both, independently. The file sink stays purely optional (as before);
/// the tap broadcast is unconditional -- `send` only errors when there are
/// currently no subscribers (no mounts/remotes configured, or none of their
/// output tasks have subscribed yet), which is not a real error and is
/// deliberately ignored.
struct OutputSink {
    file: Option<std::fs::File>,
    tap: broadcast::Sender<Arc<Vec<f32>>>,
}

impl OutputSink {
    fn write_all(&mut self, samples: &[f32]) {
        if samples.is_empty() {
            return;
        }
        if let Some(f) = self.file.as_mut() {
            let mut bytes = Vec::with_capacity(samples.len() * 4);
            for s in samples {
                bytes.extend_from_slice(&s.to_le_bytes());
            }
            if let Err(e) = f.write_all(&bytes) {
                tracing::error!("failed writing to pipeline output sink: {e}");
            }
        }
        let _ = self.tap.send(Arc::new(samples.to_vec()));
    }
}

/// Opens (creating/truncating) the raw-PCM output sink. Deliberately a raw
/// interleaved-f32-LE stream rather than a WAV file: a WAV header needs a
/// final byte count written at close time, which doesn't fit a
/// long-running, never-finalized live stream. (The `--crossfade-test` CLI
/// path, which *does* run to completion, uses proper WAV output via
/// `hound` instead -- see `main.rs`.)
fn open_output_sink(path: &str) -> Option<std::fs::File> {
    match std::fs::File::create(Path::new(path)) {
        Ok(f) => {
            tracing::info!(
                "pipeline output sink: raw f32 PCM @ {PIPELINE_SAMPLE_RATE}Hz stereo -> {path}"
            );
            Some(f)
        }
        Err(e) => {
            tracing::error!("failed to create pipeline output sink '{path}': {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- pacing_sleep_duration (pure; no runtime, no real sleeping) --------

    #[test]
    fn no_frames_emitted_yet_sleeps_for_the_chunks_own_duration() {
        // Right at stream start (elapsed = 0), a 44100-frame (1 second)
        // chunk at 44100Hz should ask for a full 1-second sleep.
        let d = pacing_sleep_duration(Duration::ZERO, 0, 44_100, 44_100);
        assert_eq!(d, Duration::from_secs(1));
    }

    #[test]
    fn caught_up_exactly_sleeps_for_only_the_new_chunk() {
        // 1 second already emitted and 1 second of wall clock has already
        // elapsed (perfectly paced so far); a further half-second chunk
        // should ask for exactly a half-second sleep.
        let d = pacing_sleep_duration(Duration::from_secs(1), 44_100, 22_050, 44_100);
        assert_eq!(d, Duration::from_millis(500));
    }

    #[test]
    fn falling_behind_wall_clock_returns_zero_not_negative() {
        // 1 second of audio emitted so far, but 5 seconds of wall clock
        // have already elapsed (e.g. a slow decode/mix stall) -- a further
        // 1-second chunk's target (2s) is already well in the past, so the
        // correct behavior is "don't sleep at all", not any attempt to
        // "catch up" by skipping or dropping audio.
        let d = pacing_sleep_duration(Duration::from_secs(5), 44_100, 44_100, 44_100);
        assert_eq!(d, Duration::ZERO);
    }

    #[test]
    fn exactly_on_target_sleeps_zero() {
        let d = pacing_sleep_duration(Duration::from_secs(2), 44_100, 44_100, 44_100);
        assert_eq!(d, Duration::ZERO);
    }

    #[test]
    fn zero_length_chunk_sleeps_zero() {
        let d = pacing_sleep_duration(Duration::ZERO, 0, 0, 44_100);
        assert_eq!(d, Duration::ZERO);
    }

    // --- StreamClock (thin wrapper around pacing_sleep_duration) -----------

    #[test]
    fn stream_clock_advance_accumulates_frames_emitted() {
        let start = tokio::time::Instant::now();
        let mut clock = StreamClock::new(start);
        assert_eq!(clock.frames_emitted, 0);
        clock.advance(44_100);
        assert_eq!(clock.frames_emitted, 44_100);
        clock.advance(22_050);
        assert_eq!(clock.frames_emitted, 66_150);
    }

    #[test]
    fn stream_clock_sleep_before_chunk_matches_pure_function() {
        let start = tokio::time::Instant::now();
        let mut clock = StreamClock::new(start);
        clock.advance(44_100); // pretend 1 second already emitted

        // "now" is still exactly `start` (no real time has passed), so the
        // clock is a full second ahead of wall clock: a further 1-second
        // chunk should demand roughly a 2-second sleep.
        let d = clock.sleep_before_chunk(start, 44_100, 44_100);
        assert_eq!(d, Duration::from_secs(2));
    }

    #[test]
    fn stream_clock_now_after_start_reduces_required_sleep() {
        let start = tokio::time::Instant::now();
        let mut clock = StreamClock::new(start);
        clock.advance(44_100); // 1 second of audio already emitted

        // Simulate half a second of real wall-clock time having actually
        // passed since `start`.
        let now = start + Duration::from_millis(500);
        let d = clock.sleep_before_chunk(now, 44_100, 44_100);
        assert_eq!(d, Duration::from_millis(1500));
    }
}
