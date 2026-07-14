//! Live playback pipeline orchestration: ties together `queue.rs`,
//! `autodj.rs`, `decode.rs`, `prepare.rs`, `crossfade.rs`, and `feedback.rs`
//! into the actual running engine loop that replaces Phase 2's demo
//! `nextsong_loop`.
//!
//! **Timing model (documented simplification):** there is no live output
//! device or Icecast connection yet (both are later phases) to pace
//! playback against in real time -- this phase's output is a local file
//! sink only. This loop is therefore driven by *buffer position* (how many
//! frames of the current track have been written to the output sink)
//! rather than either a fixed poll timer or genuine wall-clock real-time
//! pacing; real-time pacing is deferred to whichever later phase adds a
//! real-time output sink (Icecast/Shoutcast, Phase 5). The AutoDJ lookahead
//! requirement ("fetch when `default_fade` + a small buffer remains") is
//! satisfied structurally instead of temporally: the next track is always
//! fully resolved, decoded, and prepared *before* the crossfade transition
//! into it is computed, mirroring the "decode has time to prepare" intent
//! without a wall clock to race against.

use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use crate::autodj;
use crate::callbacks::CallbackClient;
use crate::config::EngineConfig;
use crate::control::ControlSignals;
use crate::crossfade::{self, CrossfadeMode, CrossfadeParams, CrossfadeThresholds};
use crate::decode::PIPELINE_SAMPLE_RATE;
use crate::feedback::FeedbackDedup;
use crate::queue::TrackQueues;

pub struct Pipeline {
    client: Arc<CallbackClient>,
    queues: Arc<TrackQueues>,
    /// Shared `/skip` + `/metadata` control-API signal handle -- see
    /// `control.rs`'s module doc. Polled (non-blockingly) once per loop
    /// iteration below.
    control: Arc<ControlSignals>,
    replaygain_enabled: bool,
    fallback_file_path: Option<String>,
    crossfade_mode: CrossfadeMode,
    thresholds: CrossfadeThresholds,
    default_fade_secs: f64,
    cross_window_secs: f64,
    output_path: Option<String>,
    feedback: FeedbackDedup,
}

impl Pipeline {
    pub fn new(
        client: Arc<CallbackClient>,
        queues: Arc<TrackQueues>,
        control: Arc<ControlSignals>,
        cfg: &EngineConfig,
    ) -> Self {
        Self {
            client,
            queues,
            control,
            replaygain_enabled: cfg.station.replaygain_enabled,
            fallback_file_path: cfg.paths.fallback_file_path.clone(),
            crossfade_mode: cfg.crossfade.mode(),
            thresholds: cfg.crossfade.thresholds(),
            default_fade_secs: cfg.crossfade.fade_seconds,
            cross_window_secs: cfg.crossfade.fade_seconds.max(crossfade::DEFAULT_CROSS_SECONDS),
            output_path: cfg.paths.pipeline_output_path.clone(),
            feedback: FeedbackDedup::new(),
        }
    }

    /// Runs forever (until the process is torn down), matching the
    /// lifecycle of Phase 2's `nextsong_loop`.
    pub async fn run(mut self) {
        let mut output = self.output_path.as_deref().and_then(open_output_sink);
        if self.output_path.is_some() && output.is_none() {
            tracing::error!(
                "failed to open pipeline output sink; continuing without file output"
            );
        }

        let mut current = autodj::fetch_next_track(
            &self.client,
            &self.queues,
            self.replaygain_enabled,
            self.fallback_file_path.as_deref(),
        )
        .await;
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

            let window_frames =
                (self.cross_window_secs * PIPELINE_SAMPLE_RATE as f64).round() as usize;

            let total_frames = current.decoded.frames();
            let mut body_end_frames =
                total_frames.saturating_sub(window_frames).max(played_frames);
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
            // defeat the point of skipping. Because this pipeline has no
            // real-time output pacing yet (see this file's module doc), the
            // practical effect of a skip signal in this phase is "the next
            // chunk of pipeline output jumps straight to the crossfade",
            // not "the currently-streaming audio audibly cuts short in real
            // time" -- that distinction only matters once a real-time-paced
            // output sink exists (Phase 5). This is a deliberate, documented
            // scope simplification, not a bug.
            if self.control.take_skip() {
                tracing::info!("skip requested; abandoning remainder of current track body");
                body_end_frames = played_frames;
                tail_end_frames = (played_frames + window_frames).min(total_frames);
            }

            write_frame_range(&mut output, &current.decoded.samples, played_frames, body_end_frames);

            // Resolve/decode/prepare the next track now -- see module doc
            // for the buffer-position-driven lookahead rationale.
            let next = autodj::fetch_next_track(
                &self.client,
                &self.queues,
                self.replaygain_enabled,
                self.fallback_file_path.as_deref(),
            )
            .await;

            // SPEC.md step 8: fire feedback at the point the new track's
            // audio starts becoming audible -- the start of the crossfade
            // transition into it -- rather than after the full crossfade
            // completes.
            self.feedback.maybe_send(&self.client, &next).await;

            let old_tail = &current.decoded.samples[body_end_frames * 2..tail_end_frames * 2];
            let head_frames = window_frames.min(next.decoded.frames());
            let new_head = &next.decoded.samples[..head_frames * 2];

            let fade_out = current.fade_out_override.unwrap_or(self.default_fade_secs);
            let fade_in = next.fade_in_override.unwrap_or(self.default_fade_secs);

            let params = CrossfadeParams {
                mode: self.crossfade_mode,
                fade_in_secs: fade_in,
                fade_out_secs: fade_out,
                thresholds: self.thresholds,
            };

            let mixed = crossfade::mix_transition(old_tail, new_head, PIPELINE_SAMPLE_RATE, &params);
            write_all(&mut output, &mixed);

            current = next;
            played_frames = head_frames;
        }
    }
}

fn write_frame_range(
    output: &mut Option<std::fs::File>,
    samples: &[f32],
    from_frame: usize,
    to_frame: usize,
) {
    if from_frame >= to_frame {
        return;
    }
    let slice = &samples[from_frame * 2..to_frame * 2];
    write_all(output, slice);
}

fn write_all(output: &mut Option<std::fs::File>, samples: &[f32]) {
    if let Some(f) = output.as_mut() {
        let mut bytes = Vec::with_capacity(samples.len() * 4);
        for s in samples {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        if let Err(e) = f.write_all(&bytes) {
            tracing::error!("failed writing to pipeline output sink: {e}");
        }
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
