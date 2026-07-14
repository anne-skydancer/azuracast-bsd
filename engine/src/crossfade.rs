//! Crossfade transition mixing, per `engine/SPEC.md` C.5.
//!
//! SPEC.md C.5's dispatch has a first branch ("to-live special case") that
//! only applies when the live-DJ harbor source has just become ready
//! (`azuracast.to_live()`) -- Phase 4 wires this up via `CrossfadeParams::to_live`
//! and `to_live_transition` below, gated by `harbor.rs`/`pipeline.rs`'s
//! integration (see `pipeline.rs`'s module doc for exactly when `to_live` is
//! set: the single AutoDJ-to-live transition, never live-to-live or
//! live-to-AutoDJ). Everything below that first branch is the three
//! station-wide crossfade modes (`smart`/`normal`/disabled) and, within
//! `smart`, its own five dB-comparison branches (`cross.smart`'s exact
//! logic), in the exact priority order given.
//!
//! This operates on full in-memory sample buffers (the outgoing track's
//! tail window and the incoming track's head window) and produces the
//! mixed transition segment -- no real-time/streaming DSP, matching the
//! "full in-memory decode" approach used throughout this phase.

use ebur128::{EbuR128, Mode};

use crate::decode::PIPELINE_CHANNELS;

pub const DEFAULT_HIGH: f64 = -15.0;
pub const DEFAULT_MEDIUM: f64 = -32.0;
pub const DEFAULT_MARGIN: f64 = 8.0;
/// SPEC.md A.1: `crossfade` default 2.0s (`default_fade`).
pub const DEFAULT_FADE_SECONDS: f64 = 2.0;
/// SPEC.md A.1 `getCrossfadeDuration()`: `round(crossfade * 1.5, 2)`.
pub const DEFAULT_CROSS_SECONDS: f64 = 3.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrossfadeMode {
    /// `cross.smart` -- dB-aware branch dispatch (see `select_smart_branch`).
    Smart,
    /// `cross.simple` -- plain unconditional crossfade, no dB analysis.
    Normal,
    /// Crossfade disabled station-wide. Per SPEC.md C.5 point 3, this is
    /// *not* a hard cut: `default_fade`-duration fade-in/fade-out `add()`
    /// mixing still happens, just without any dB-aware branch selection.
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CrossfadeThresholds {
    pub high: f64,
    pub medium: f64,
    pub margin: f64,
}

impl Default for CrossfadeThresholds {
    fn default() -> Self {
        Self {
            high: DEFAULT_HIGH,
            medium: DEFAULT_MEDIUM,
            margin: DEFAULT_MARGIN,
        }
    }
}

/// `cross.smart`'s five branches, in SPEC.md C.5 priority order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmartBranch {
    /// Branch 1: both tracks quiet/similar enough to overlap safely.
    FullCrossfade,
    /// Branch 2: new track significantly louder than old; fade the
    /// outgoing track only, let the new one enter at full volume.
    FadeOutOnly,
    /// Branch 3: mirror of branch 2 (old significantly louder than new).
    FadeInOnly,
    /// Branch 4: old track already near-silent -- straight cut-in, no fade.
    HardCutIn,
    /// Branch 5 (`default`): everything else -- hard sequential cut, no
    /// overlap at all.
    HardSequentialCut,
}

/// Reproduces `cross.smart`'s exact five-branch dB-comparison dispatch
/// (SPEC.md C.5), evaluated top-to-bottom, first match wins.
///
/// `a_db`/`b_db` are the measured "loudness level" of the outgoing (`a`)
/// and incoming (`b`) track's analysis window, in the same units as
/// `thresholds` (see `measure_loudness_dbfs`).
pub fn select_smart_branch(a_db: f64, b_db: f64, thresholds: &CrossfadeThresholds) -> SmartBranch {
    let t = thresholds;
    if a_db <= t.medium && b_db <= t.medium && (a_db - b_db).abs() <= t.margin {
        SmartBranch::FullCrossfade
    } else if b_db >= a_db + t.margin && a_db >= t.medium && b_db <= t.high {
        SmartBranch::FadeOutOnly
    } else if a_db >= b_db + t.margin && b_db >= t.medium && a_db <= t.high {
        SmartBranch::FadeInOnly
    } else if b_db >= a_db + t.margin && a_db <= t.medium && b_db <= t.high {
        SmartBranch::HardCutIn
    } else {
        SmartBranch::HardSequentialCut
    }
}

/// Fade curve shape. SPEC.md C.5 specifies `type="sin"` for all of
/// `cross.smart`'s fading branches; `cross.simple`/disabled-mode fades
/// aren't given a shape by the spec, so linear is used there (task
/// explicitly allows this).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FadeCurve {
    Linear,
    /// Sine-shaped fade: `gain_in(t) = sin(t * pi/2)`, `gain_out(t) =
    /// cos(t * pi/2)` -- monotonic, 0/1 at the endpoints, matching
    /// Liquidsoap's `fade.in`/`fade.out` `type="sin"` shape closely enough
    /// for this phase (the exact curve Liquidsoap uses internally isn't
    /// documented in SPEC.md beyond the `"sin"` name).
    Sine,
}

impl FadeCurve {
    fn gain_in(self, t: f64) -> f32 {
        match self {
            FadeCurve::Linear => t as f32,
            FadeCurve::Sine => (t * std::f64::consts::FRAC_PI_2).sin() as f32,
        }
    }

    fn gain_out(self, t: f64) -> f32 {
        match self {
            FadeCurve::Linear => (1.0 - t) as f32,
            FadeCurve::Sine => (t * std::f64::consts::FRAC_PI_2).cos() as f32,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CrossfadeParams {
    pub mode: CrossfadeMode,
    /// Fade-in duration in seconds for the incoming track (station default
    /// `default_fade`, unless the track's own `autocue_fade_in` overrides
    /// it -- see `prepare.rs`).
    pub fade_in_secs: f64,
    /// Fade-out duration in seconds for the outgoing track.
    pub fade_out_secs: f64,
    pub thresholds: CrossfadeThresholds,
    /// SPEC.md C.5 dispatch branch 1 ("to-live special case"). When `true`,
    /// `mix_transition` ignores `mode`/`thresholds` entirely -- see
    /// `to_live_transition`. Set by `pipeline.rs` exactly on the single
    /// transition where the live-DJ harbor source just became the active
    /// source (never for live-to-live continuation or live-to-AutoDJ, which
    /// SPEC.md C.5 explicitly leaves ungoverned by this branch).
    pub to_live: bool,
}

/// Measures a "loudness level" for `samples` (interleaved PCM at
/// `sample_rate`/`channels`) using `ebur128`'s short-term (3s window)
/// loudness measurement.
///
/// **Measurement choice, documented:** EBU R128 momentary loudness uses a
/// *fixed* 400ms integration window, which is much shorter than the ~2-4s
/// analysis window this crossfade decision wants to look at (roughly the
/// fade duration). Short-term loudness's 3-second window is a much closer
/// match, so that's what's used here rather than momentary, even though
/// SPEC.md's own `db_level` (a plain RMS-power-in-dB measurement, not an
/// EBU R128 K-weighted LUFS figure) isn't identical to either. This is a
/// deliberate substitute metric -- LUFS-via-ebur128 approximates "how loud
/// does this sound" well enough for the branch thresholds' *shape* to
/// still make sensible decisions, even though the absolute numbers won't
/// match Liquidsoap's `rms()`-based `db_level` bit-for-bit.
pub fn measure_loudness_dbfs(samples: &[f32], channels: u32, sample_rate: u32) -> f64 {
    if samples.is_empty() || channels == 0 {
        return f64::NEG_INFINITY;
    }
    let mut state = match EbuR128::new(channels, sample_rate, Mode::S) {
        Ok(s) => s,
        Err(_) => return f64::NEG_INFINITY,
    };
    if state.add_frames_f32(samples).is_err() {
        return f64::NEG_INFINITY;
    }
    match state.loudness_shortterm() {
        Ok(l) if l.is_finite() => l,
        _ => f64::NEG_INFINITY,
    }
}

/// Produces the mixed transition segment given the outgoing track's tail
/// window and the incoming track's head window (both interleaved stereo
/// PCM at the pipeline sample rate). The two windows need not be the same
/// length; per Liquidsoap's `add()` semantics, the result is
/// `max(old_tail_frames, new_head_frames)` frames long -- once the shorter
/// of the two ends, the mix simply continues with whichever one is longer,
/// unfaded, for the remainder (except the `HardSequentialCut`/disabled
/// "to-live"-style branches, which concatenate instead of overlapping).
pub fn mix_transition(
    old_tail: &[f32],
    new_head: &[f32],
    sample_rate: u32,
    params: &CrossfadeParams,
) -> Vec<f32> {
    let channels = PIPELINE_CHANNELS as usize;

    if params.to_live {
        return to_live_transition(
            old_tail,
            new_head,
            channels,
            sample_rate,
            params.fade_out_secs,
            params.fade_in_secs,
        );
    }

    match params.mode {
        CrossfadeMode::Disabled => mix_add_fade(
            old_tail,
            new_head,
            channels,
            sample_rate,
            params.fade_out_secs,
            params.fade_in_secs,
            FadeCurve::Linear,
        ),
        CrossfadeMode::Normal => mix_add_fade(
            old_tail,
            new_head,
            channels,
            sample_rate,
            params.fade_out_secs,
            params.fade_in_secs,
            FadeCurve::Linear,
        ),
        CrossfadeMode::Smart => {
            let a_db = measure_loudness_dbfs(old_tail, channels as u32, sample_rate);
            let b_db = measure_loudness_dbfs(new_head, channels as u32, sample_rate);
            let branch = select_smart_branch(a_db, b_db, &params.thresholds);
            tracing::info!(
                "crossfade: a_db={a_db:.2} b_db={b_db:.2} branch={branch:?}"
            );
            match branch {
                SmartBranch::FullCrossfade => mix_add_fade(
                    old_tail,
                    new_head,
                    channels,
                    sample_rate,
                    params.fade_out_secs,
                    params.fade_in_secs,
                    FadeCurve::Sine,
                ),
                SmartBranch::FadeOutOnly => mix_add_fade(
                    old_tail,
                    new_head,
                    channels,
                    sample_rate,
                    params.fade_out_secs,
                    0.0,
                    FadeCurve::Sine,
                ),
                SmartBranch::FadeInOnly => mix_add_fade(
                    old_tail,
                    new_head,
                    channels,
                    sample_rate,
                    0.0,
                    params.fade_in_secs,
                    FadeCurve::Sine,
                ),
                SmartBranch::HardCutIn => mix_add_fade(
                    old_tail,
                    new_head,
                    channels,
                    sample_rate,
                    0.0,
                    0.0,
                    FadeCurve::Sine,
                ),
                SmartBranch::HardSequentialCut => sequence(old_tail, new_head),
            }
        }
    }
}

/// `add(normalize=false, [fade.in(...new), fade.out(...old)])`: sums the
/// (possibly faded) outgoing tail and incoming head sample-for-sample,
/// continuing with whichever is longer once the shorter one runs out.
fn mix_add_fade(
    old_tail: &[f32],
    new_head: &[f32],
    channels: usize,
    sample_rate: u32,
    fade_out_secs: f64,
    fade_in_secs: f64,
    curve: FadeCurve,
) -> Vec<f32> {
    let old_frames = old_tail.len() / channels;
    let new_frames = new_head.len() / channels;
    let total_frames = old_frames.max(new_frames);

    let fade_out_frames = ((fade_out_secs * sample_rate as f64).round() as usize).min(old_frames);
    let fade_in_frames = ((fade_in_secs * sample_rate as f64).round() as usize).min(new_frames);

    let mut out = vec![0.0f32; total_frames * channels];

    for f in 0..total_frames {
        if f < old_frames {
            let gain = if fade_out_frames > 0 && f + fade_out_frames >= old_frames {
                let into_fade = f + fade_out_frames - old_frames; // 0..fade_out_frames
                let t = into_fade as f64 / fade_out_frames as f64;
                curve.gain_out(t)
            } else {
                1.0
            };
            for c in 0..channels {
                out[f * channels + c] += old_tail[f * channels + c] * gain;
            }
        }
        if f < new_frames {
            let gain = if fade_in_frames > 0 && f < fade_in_frames {
                let t = f as f64 / fade_in_frames as f64;
                curve.gain_in(t)
            } else {
                1.0
            };
            for c in 0..channels {
                out[f * channels + c] += new_head[f * channels + c] * gain;
            }
        }
    }

    out
}

/// Hard sequential cut: outgoing tail plays to completion, then incoming
/// head starts -- no overlap at all (`cross.smart`'s own built-in default
/// branch, `fun(a,b) -> sequence([a,b])`).
fn sequence(old_tail: &[f32], new_head: &[f32]) -> Vec<f32> {
    let mut out = Vec::with_capacity(old_tail.len() + new_head.len());
    out.extend_from_slice(old_tail);
    out.extend_from_slice(new_head);
    out
}

/// SPEC.md C.5 dispatch branch 1 ("to-live special case"):
/// `sequence([fade.out(duration=default_fade(), old.source), fade.in(duration=default_fade(), new.source)])`
/// -- ignores the station's crossfade mode/dB-analysis entirely. The
/// outgoing track's tail fades out completely (over `fade_out_secs`), *then*
/// the incoming (live) head fades in (over `fade_in_secs`) -- sequential,
/// not overlapped/summed, unlike every other branch in this module. SPEC.md
/// doesn't name a curve shape for this branch (unlike `cross.smart`'s
/// explicit `type="sin"`), so linear is used here -- the same documented
/// convention already applied to the `Normal`/`Disabled` branches above.
fn to_live_transition(
    old_tail: &[f32],
    new_head: &[f32],
    channels: usize,
    sample_rate: u32,
    fade_out_secs: f64,
    fade_in_secs: f64,
) -> Vec<f32> {
    let mut old = old_tail.to_vec();
    apply_fade_out_in_place(&mut old, channels, sample_rate, fade_out_secs, FadeCurve::Linear);

    let mut new = new_head.to_vec();
    apply_fade_in_in_place(&mut new, channels, sample_rate, fade_in_secs, FadeCurve::Linear);

    let mut out = Vec::with_capacity(old.len() + new.len());
    out.extend_from_slice(&old);
    out.extend_from_slice(&new);
    out
}

/// Applies a fade-out curve to the trailing `fade_secs` of `samples`
/// in-place (frames before that trailing window are left untouched, at full
/// gain). Used only by `to_live_transition`, which fades each side of the
/// sequential cut independently rather than as part of an overlapping `add`
/// mix (contrast with `mix_add_fade`, which fades within a summed overlap).
fn apply_fade_out_in_place(
    samples: &mut [f32],
    channels: usize,
    sample_rate: u32,
    fade_secs: f64,
    curve: FadeCurve,
) {
    let frames = samples.len() / channels;
    let fade_frames = ((fade_secs * sample_rate as f64).round() as usize).min(frames);
    if fade_frames == 0 {
        return;
    }
    let start = frames - fade_frames;
    for f in start..frames {
        let t = (f - start) as f64 / fade_frames as f64;
        let gain = curve.gain_out(t);
        for c in 0..channels {
            samples[f * channels + c] *= gain;
        }
    }
}

/// Applies a fade-in curve to the leading `fade_secs` of `samples`
/// in-place (frames after that leading window are left untouched, at full
/// gain). See `apply_fade_out_in_place`'s doc for why this is separate from
/// `mix_add_fade`.
fn apply_fade_in_in_place(
    samples: &mut [f32],
    channels: usize,
    sample_rate: u32,
    fade_secs: f64,
    curve: FadeCurve,
) {
    let frames = samples.len() / channels;
    let fade_frames = ((fade_secs * sample_rate as f64).round() as usize).min(frames);
    if fade_frames == 0 {
        return;
    }
    for f in 0..fade_frames {
        let t = f as f64 / fade_frames as f64;
        let gain = curve.gain_in(t);
        for c in 0..channels {
            samples[f * channels + c] *= gain;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_crossfade_when_both_quiet_and_close() {
        let t = CrossfadeThresholds::default();
        let branch = select_smart_branch(-35.0, -33.0, &t);
        assert_eq!(branch, SmartBranch::FullCrossfade);
    }

    #[test]
    fn fade_out_only_when_new_much_louder() {
        let t = CrossfadeThresholds::default();
        // a >= medium (-32), b <= high (-15), b >= a + margin (8)
        let branch = select_smart_branch(-30.0, -16.0, &t);
        assert_eq!(branch, SmartBranch::FadeOutOnly);
    }

    #[test]
    fn fade_in_only_when_old_much_louder() {
        let t = CrossfadeThresholds::default();
        let branch = select_smart_branch(-16.0, -30.0, &t);
        assert_eq!(branch, SmartBranch::FadeInOnly);
    }

    #[test]
    fn hard_cut_in_when_old_near_silent() {
        let t = CrossfadeThresholds::default();
        // a <= medium, b <= high, b >= a + margin
        let branch = select_smart_branch(-60.0, -20.0, &t);
        assert_eq!(branch, SmartBranch::HardCutIn);
    }

    #[test]
    fn falls_through_to_hard_sequential_cut() {
        let t = CrossfadeThresholds::default();
        // Both loud (> high), no branch above matches.
        let branch = select_smart_branch(-5.0, -5.0, &t);
        assert_eq!(branch, SmartBranch::HardSequentialCut);
    }

    #[test]
    fn mix_add_fade_sums_and_extends_with_longer() {
        let channels = 2usize;
        let old = vec![1.0f32; 4 * channels]; // 4 frames
        let new = vec![1.0f32; 8 * channels]; // 8 frames
        let out = mix_add_fade(&old, &new, channels, 4, 0.0, 0.0, FadeCurve::Linear);
        assert_eq!(out.len(), 8 * channels);
        // Overlap region: 1.0 (old) + 1.0 (new) = 2.0.
        assert!((out[0] - 2.0).abs() < 1e-6);
        // Tail region (only new remains): 1.0.
        assert!((out[6 * channels] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn sequence_concatenates_without_overlap() {
        let old = vec![1.0f32, 1.0, 1.0, 1.0];
        let new = vec![2.0f32, 2.0];
        let out = sequence(&old, &new);
        assert_eq!(out, vec![1.0, 1.0, 1.0, 1.0, 2.0, 2.0]);
    }

    #[test]
    fn to_live_dispatch_ignores_mode_and_produces_no_overlap() {
        // `mode: Smart` with dB levels that would normally select
        // `FullCrossfade` (overlapping `add`, length = max(old, new)) --
        // `to_live: true` must override this entirely and dispatch to the
        // sequential (non-overlapping) to-live transition instead, whose
        // output length is old.len() + new.len(), not max(old, new).
        let channels = 2usize;
        let old_tail = vec![0.1f32; 4 * channels];
        let new_head = vec![0.1f32; 4 * channels];
        let params = CrossfadeParams {
            mode: CrossfadeMode::Smart,
            fade_in_secs: 0.0,
            fade_out_secs: 0.0,
            thresholds: CrossfadeThresholds::default(),
            to_live: true,
        };
        let out = mix_transition(&old_tail, &new_head, 4, &params);
        assert_eq!(out.len(), old_tail.len() + new_head.len());
    }

    #[test]
    fn to_live_fades_old_out_and_new_in_sequentially() {
        let channels = 2usize;
        let sample_rate = 4u32; // 4 frames == 1 second, for round numbers
        let old_tail = vec![1.0f32; 4 * channels]; // 1s, full volume
        let new_head = vec![1.0f32; 4 * channels]; // 1s, full volume
        let params = CrossfadeParams {
            mode: CrossfadeMode::Normal, // must be ignored entirely
            fade_in_secs: 1.0,
            fade_out_secs: 1.0,
            thresholds: CrossfadeThresholds::default(),
            to_live: true,
        };
        let out = mix_transition(&old_tail, &new_head, sample_rate, &params);

        // No overlap: total length is old + new, unlike a crossfade add.
        assert_eq!(out.len(), old_tail.len() + new_head.len());

        // Old side: fade-out starts at gain 1.0 (first frame of the
        // fade-out window) and ends near 0.0 (last frame before the cut).
        assert!((out[0] - 1.0).abs() < 1e-6, "old tail should start unfaded");
        let last_old_frame = 3 * channels;
        assert!(out[last_old_frame] < 0.3, "old tail should be nearly silent by its end");

        // New side: fade-in starts near 0.0 and ends at gain 1.0.
        let new_start = old_tail.len();
        assert!(out[new_start] < 0.3, "new head should start nearly silent");
        let last_new_frame = new_start + 3 * channels;
        assert!(
            (out[last_new_frame] - 1.0).abs() < 0.3,
            "new head should approach full volume by its end"
        );
    }
}
