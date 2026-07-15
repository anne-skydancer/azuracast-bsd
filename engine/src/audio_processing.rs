//! Audio post-processing: the engine's replacement for the historical
//! Liquidsoap integration's `writePostProcessingSection` (SPEC.md), which
//! dispatched on `AudioProcessingMethods` to one of three `.liq` chains --
//! `nrj` (built-in `normalize`/`compress.exponential` DSP), `master_me` (a
//! LADSPA mastering plugin), or `stereo_tool` (a piped external binary).
//!
//! **`master_me` has no equivalent here and never will.** It required
//! Liquidsoap's LADSPA plugin host calling into `ladspa.master_me`, a
//! complex multiband mastering chain with ~60 tunable parameters. There is
//! no LADSPA host in this engine, and the algorithm itself is not
//! documented anywhere available to reimplement against -- porting it would
//! mean reverse-engineering a proprietary-grade mastering algorithm from
//! scratch, not writing new code from a spec. This was dropped at the
//! PHP/UI layer (`AudioProcessingMethods::MasterMe`, `MasterMePresets`, and
//! the associated station form fields no longer exist) rather than faked
//! here.
//!
//! `nrj` and `stereo_tool` are implemented below, in [`NrjProcessor`] and
//! [`StereoToolProcessor`] respectively -- see each type's doc for exactly
//! what is and isn't a faithful port of the original.

use std::process::Stdio;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use crate::config::AudioProcessingConfig;
use crate::decode::{PIPELINE_CHANNELS, PIPELINE_SAMPLE_RATE};

/// Dispatches to whichever post-processing method (if any) the station has
/// configured. `Pipeline` holds one of these for its whole lifetime (see
/// `pipeline.rs`) and calls `process()` on every chunk it's about to emit,
/// gated by `AudioProcessingConfig::include_live` at the call site.
pub enum AudioProcessor {
    None,
    Nrj(NrjProcessor),
    StereoTool(StereoToolProcessor),
}

impl AudioProcessor {
    /// Builds the configured processor. Never fails outright: an
    /// unrecognized `method`, a `stereo_tool` config missing its
    /// binary/preset path, or a `stereo_tool` subprocess that fails to
    /// spawn all fall back to `AudioProcessor::None` (logged), rather than
    /// aborting engine startup over a post-processing misconfiguration.
    pub fn from_config(cfg: &AudioProcessingConfig) -> Self {
        match cfg.method.as_str() {
            "nrj" => {
                tracing::info!("audio post-processing: nrj (normalize + compress)");
                AudioProcessor::Nrj(NrjProcessor::new(PIPELINE_SAMPLE_RATE))
            }
            "stereo_tool" => {
                let (binary, preset) =
                    match (&cfg.stereo_tool_binary, &cfg.stereo_tool_preset_path) {
                        (Some(b), Some(p)) => (b, p),
                        _ => {
                            tracing::warn!(
                                "audio_processing.method = \"stereo_tool\" but binary/preset path \
                                 missing from config; disabling post-processing"
                            );
                            return AudioProcessor::None;
                        }
                    };

                match StereoToolProcessor::spawn(
                    binary,
                    preset,
                    cfg.stereo_tool_license_key.as_deref(),
                ) {
                    Ok(proc) => {
                        tracing::info!("audio post-processing: stereo_tool ({binary})");
                        AudioProcessor::StereoTool(proc)
                    }
                    Err(e) => {
                        tracing::error!(
                            "failed to start stereo_tool subprocess, disabling post-processing: {e}"
                        );
                        AudioProcessor::None
                    }
                }
            }
            "none" => AudioProcessor::None,
            other => {
                tracing::warn!(
                    "unrecognized audio_processing.method \"{other}\"; treating as \"none\""
                );
                AudioProcessor::None
            }
        }
    }

    /// Applies this processor's transform to `samples` (interleaved stereo
    /// `f32`) in place. A no-op for `AudioProcessor::None`. May await
    /// briefly for `StereoTool` (subprocess I/O) -- see that type's doc.
    pub async fn process(&mut self, samples: &mut [f32]) {
        match self {
            AudioProcessor::None => {}
            AudioProcessor::Nrj(p) => p.process(samples),
            AudioProcessor::StereoTool(p) => p.process(samples).await,
        }
    }
}

// --- nrj: normalize + compress ---------------------------------------------

/// Target RMS level for the normalizer, in dBFS. Matches the historical
/// `.liq` call's `target = 0.` exactly.
const NRJ_TARGET_DB: f64 = 0.0;
/// RMS analysis window. Matches `window = 0.03` exactly.
const NRJ_WINDOW_SECS: f64 = 0.03;
/// Gain range: attenuate-only (never boosts above unity), matching
/// `gain_min = -16.` / `gain_max = 0.` exactly.
const NRJ_GAIN_MIN_DB: f64 = -16.0;
const NRJ_GAIN_MAX_DB: f64 = 0.0;
/// Compressor envelope time constant. Matches `compress.exponential(mu =
/// 1.0)` exactly.
const NRJ_COMPRESSOR_MU_SECS: f64 = 1.0;
/// How fast the normalizer's smoothed gain chases its instantaneous target.
/// **Not extracted from anywhere** -- Liquidsoap's own internal gain-change
/// smoothing for `normalize` isn't part of the public parameters the
/// historical `.liq` call passed (it didn't override `up`/`down`, so used
/// Liquidsoap's own defaults, which aren't available to verify here). This
/// value is this implementation's own reasonable choice: fast enough to
/// track real loudness changes, slow enough not to audibly "pump" on
/// individual transients.
const NRJ_GAIN_SMOOTH_SECS: f64 = 0.05;
/// Floor for `linear_to_db` to avoid `log10(0)`; anything this quiet is
/// treated as silence for gain-computation purposes.
const SILENCE_FLOOR_DB: f64 = -90.0;

fn linear_to_db(linear: f64) -> f64 {
    if linear <= 0.0 {
        return SILENCE_FLOOR_DB;
    }
    (20.0 * linear.log10()).max(SILENCE_FLOOR_DB)
}

fn db_to_linear(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

/// Pure: the instantaneous (unsmoothed) gain in dB that would bring
/// `rms_linear` to `target_db`, clamped to `[gain_min_db, gain_max_db]`.
fn nrj_target_gain_db(rms_linear: f64, target_db: f64, gain_min_db: f64, gain_max_db: f64) -> f64 {
    let rms_db = linear_to_db(rms_linear);
    (target_db - rms_db).clamp(gain_min_db, gain_max_db)
}

/// Pure: one step of one-pole smoothing from `previous` toward `target`,
/// given a per-step coefficient `alpha` (see `one_pole_alpha`).
fn smooth_toward(previous: f64, target: f64, alpha: f64) -> f64 {
    previous + (target - previous) * (1.0 - alpha)
}

/// Pure: the one-pole IIR coefficient for time constant `tau_secs`,
/// evaluated once per `frames_per_step` frames at `sample_rate` (so a
/// per-chunk update passes the chunk's own frame count, not `1`).
fn one_pole_alpha(tau_secs: f64, sample_rate: u32, frames_per_step: usize) -> f64 {
    if tau_secs <= 0.0 || sample_rate == 0 {
        return 0.0;
    }
    let dt = frames_per_step as f64 / sample_rate as f64;
    (-dt / tau_secs).exp()
}

/// Pure: one envelope-follower step -- `alpha`-smoothed toward
/// `chunk_peak_abs` (see `compressor_gain` for how this becomes a gain).
fn compressor_envelope_step(previous_envelope: f64, chunk_peak_abs: f64, alpha: f64) -> f64 {
    smooth_toward(previous_envelope, chunk_peak_abs.max(0.0), alpha)
}

/// Pure: attenuate-only gain from a tracked envelope -- reduces gain when
/// the envelope exceeds unity (0 dBFS), never boosts quiet signal. This is
/// what makes it a compressor/limiter rather than a second normalizer.
fn compressor_gain(envelope: f64) -> f64 {
    if envelope > 1.0 {
        1.0 / envelope
    } else {
        1.0
    }
}

/// Normalize (RMS-window automatic gain, attenuate-only) followed by a
/// single-pole "exponential" envelope-follower compressor/limiter -- the
/// *shape* of Liquidsoap's historical `normalize(target=0., window=0.03,
/// gain_min=-16., gain_max=0.)` + `compress.exponential(mu=1.0)` chain (see
/// this module's doc and SPEC.md's extracted `writePostProcessingSection`).
///
/// **Not a bit-exact port.** What's preserved exactly: the target level (0
/// dBFS RMS), the analysis window (30ms), the gain range (attenuate-only,
/// -16dB to 0dB), and the compressor's time constant (mu = 1.0s). The
/// normalizer's gain-smoothing rate (`NRJ_GAIN_SMOOTH_SECS`) is this
/// implementation's own choice -- see that constant's doc.
pub struct NrjProcessor {
    sample_rate: u32,
    window_frames: usize,
    window: std::collections::VecDeque<f64>,
    window_sum_sq: f64,
    gain_db: f64,
    envelope: f64,
}

impl NrjProcessor {
    pub fn new(sample_rate: u32) -> Self {
        let window_frames = ((NRJ_WINDOW_SECS * sample_rate as f64).round() as usize).max(1);
        Self {
            sample_rate,
            window_frames,
            window: std::collections::VecDeque::with_capacity(window_frames),
            window_sum_sq: 0.0,
            // Start at unity (no attenuation) rather than `gain_min`, so
            // playback doesn't open unexpectedly quiet before the RMS
            // window has filled.
            gain_db: NRJ_GAIN_MAX_DB,
            envelope: 0.0,
        }
    }

    /// Applies normalize+compress to `samples` (interleaved stereo `f32`)
    /// in place. Chunk-level (not per-sample) gain updates: the
    /// normalizer's smoothed gain and the compressor's envelope are each
    /// recomputed once per call from the chunk's aggregate stats, then
    /// applied uniformly across the chunk. That trades a little precision
    /// against true per-sample adaptation for simplicity, and is reasonable
    /// at the pipeline's actual chunk sizes (well within the 30ms analysis
    /// window this is modeled on).
    pub fn process(&mut self, samples: &mut [f32]) {
        if samples.is_empty() {
            return;
        }

        let channels = PIPELINE_CHANNELS as usize;
        let frames = samples.len() / channels;

        for frame in samples.chunks_exact(channels) {
            let mag_sq: f64 =
                frame.iter().map(|s| (*s as f64) * (*s as f64)).sum::<f64>() / channels as f64;
            self.window.push_back(mag_sq);
            self.window_sum_sq += mag_sq;
            if self.window.len() > self.window_frames {
                if let Some(old) = self.window.pop_front() {
                    self.window_sum_sq -= old;
                }
            }
        }

        let rms = if self.window.is_empty() {
            0.0
        } else {
            (self.window_sum_sq / self.window.len() as f64).max(0.0).sqrt()
        };

        let target_gain_db =
            nrj_target_gain_db(rms, NRJ_TARGET_DB, NRJ_GAIN_MIN_DB, NRJ_GAIN_MAX_DB);
        let gain_alpha = one_pole_alpha(NRJ_GAIN_SMOOTH_SECS, self.sample_rate, frames);
        self.gain_db = smooth_toward(self.gain_db, target_gain_db, gain_alpha);
        let normalize_gain = db_to_linear(self.gain_db);

        let mut chunk_peak = 0.0f64;
        for s in samples.iter() {
            chunk_peak = chunk_peak.max(((*s as f64) * normalize_gain).abs());
        }

        let comp_alpha = one_pole_alpha(NRJ_COMPRESSOR_MU_SECS, self.sample_rate, frames);
        self.envelope = compressor_envelope_step(self.envelope, chunk_peak, comp_alpha);
        let total_gain = (normalize_gain * compressor_gain(self.envelope)) as f32;

        for s in samples.iter_mut() {
            *s *= total_gain;
        }
    }
}

// --- stereo_tool: external subprocess pipe ----------------------------------

/// Pipes audio through an operator-installed, separately-licensed
/// `stereo_tool` CLI binary as a persistent subprocess, mirroring the
/// invocation Liquidsoap's own `pipe()` operator used historically:
/// `<binary> --silent - - -s <preset_path> [-k <license_key>]` (`-` `-`
/// meaning "read from stdin, write to stdout"; `--silent` suppresses the
/// tool's own log chatter). The engine does not reimplement Stereo Tool's
/// processing -- only plumbing audio through the operator's own binary.
///
/// **Format assumption, not independently verified.** This pipes raw
/// interleaved stereo `f32` LE PCM at the pipeline's native sample rate --
/// the same format already used for ffmpeg's stdin in `output.rs` -- on the
/// assumption Stereo Tool's CLI accepts/produces the same raw format
/// Liquidsoap fed it internally (the historical `.liq` `pipe()` call passed
/// no explicit format flags to the binary, consistent with a raw-PCM
/// passthrough). Stereo Tool is closed-source; its exact wire format isn't
/// confirmed against real documentation here. If the true format differs,
/// audio will come out corrupted or silent rather than merely unprocessed --
/// **verify against the real binary during jail integration testing** before
/// relying on this in production.
pub struct StereoToolProcessor {
    child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
}

impl StereoToolProcessor {
    pub fn spawn(binary: &str, preset_path: &str, license_key: Option<&str>) -> Result<Self, String> {
        let mut cmd = Command::new(binary);
        cmd.arg("--silent").arg("-").arg("-").arg("-s").arg(preset_path);

        if let Some(key) = license_key {
            if !key.is_empty() {
                cmd.arg("-k").arg(key);
            }
        }

        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::null());

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("failed to spawn '{binary}': {e}"))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "spawned stereo_tool process has no stdin handle".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "spawned stereo_tool process has no stdout handle".to_string())?;

        tracing::info!(
            "stereo_tool subprocess started (pid {})",
            child.id().unwrap_or(0)
        );

        Ok(Self { child, stdin, stdout })
    }

    /// Writes `samples` to the subprocess's stdin as raw `f32` LE bytes,
    /// then reads back exactly as many bytes from stdout, overwriting
    /// `samples` in place. Blocks (via `.await`) until the subprocess
    /// produces that much output -- if the tool buffers internally (e.g.
    /// lookahead), this applies natural backpressure to the pipeline rather
    /// than desyncing frame counts. It does **not** time out or fall back:
    /// a hung or crashed subprocess stalls the whole pipeline. Acceptable
    /// for this phase; a watchdog/restart policy is a reasonable follow-up
    /// once this is exercised against the real binary.
    pub async fn process(&mut self, samples: &mut [f32]) {
        if samples.is_empty() {
            return;
        }

        let out_bytes = samples_to_le_bytes(samples);

        if let Err(e) = self.stdin.write_all(&out_bytes).await {
            tracing::error!("stereo_tool: failed writing to subprocess stdin: {e}");
            return;
        }
        if let Err(e) = self.stdin.flush().await {
            tracing::error!("stereo_tool: failed flushing subprocess stdin: {e}");
            return;
        }

        let mut in_bytes = vec![0u8; samples.len() * 4];
        if let Err(e) = self.stdout.read_exact(&mut in_bytes).await {
            tracing::error!(
                "stereo_tool: failed reading from subprocess stdout (process pid {}): {e}",
                self.child.id().unwrap_or(0)
            );
            return;
        }

        le_bytes_into_samples(&in_bytes, samples);
    }
}

/// Pure: interleaved `f32` samples -> raw little-endian bytes (4 bytes per
/// sample). Split out from `StereoToolProcessor::process` so the
/// byte-marshalling itself is unit-testable without spawning anything.
fn samples_to_le_bytes(samples: &[f32]) -> Vec<u8> {
    let mut out = vec![0u8; samples.len() * 4];
    for (i, s) in samples.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&s.to_le_bytes());
    }
    out
}

/// Pure: raw little-endian bytes (4 bytes per sample) -> overwrites `out`
/// in place with the decoded `f32` samples. `bytes.len()` must equal
/// `out.len() * 4` (the only caller, `process`, guarantees this by
/// construction).
fn le_bytes_into_samples(bytes: &[u8], out: &mut [f32]) {
    for (i, s) in out.iter_mut().enumerate() {
        let chunk: [u8; 4] = bytes[i * 4..i * 4 + 4]
            .try_into()
            .expect("chunk is exactly 4 bytes");
        *s = f32::from_le_bytes(chunk);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- pure gain-math functions -------------------------------------

    #[test]
    fn linear_to_db_and_back_round_trip() {
        let db = linear_to_db(0.5);
        let back = db_to_linear(db);
        assert!((back - 0.5).abs() < 1e-9);
    }

    #[test]
    fn linear_to_db_of_zero_hits_floor_not_negative_infinity() {
        assert_eq!(linear_to_db(0.0), SILENCE_FLOOR_DB);
    }

    #[test]
    fn nrj_target_gain_clamps_loud_signal_to_gain_min() {
        // A signal at 0dBFS RMS (linear 1.0) is already at target, so with
        // no headroom needed the gain should clamp to 0dB (gain_max), not
        // demand attenuation below it incorrectly.
        let g = nrj_target_gain_db(1.0, NRJ_TARGET_DB, NRJ_GAIN_MIN_DB, NRJ_GAIN_MAX_DB);
        assert_eq!(g, 0.0);
    }

    #[test]
    fn nrj_target_gain_never_exceeds_gain_max_for_quiet_signal() {
        // A very quiet signal would mathematically need a large *positive*
        // gain to reach 0dBFS target -- but gain_max=0dB means "never
        // boost," so it should clamp to 0dB, not amplify.
        let g = nrj_target_gain_db(0.001, NRJ_TARGET_DB, NRJ_GAIN_MIN_DB, NRJ_GAIN_MAX_DB);
        assert_eq!(g, NRJ_GAIN_MAX_DB);
    }

    #[test]
    fn nrj_target_gain_clamps_to_gain_min_for_extremely_loud_signal() {
        // linear_to_db(10.0) = 20dB, well above the 16dB attenuation range,
        // so the demanded gain should clamp at gain_min rather than go
        // further negative.
        let g = nrj_target_gain_db(10.0, NRJ_TARGET_DB, NRJ_GAIN_MIN_DB, NRJ_GAIN_MAX_DB);
        assert_eq!(g, NRJ_GAIN_MIN_DB);
    }

    #[test]
    fn smooth_toward_moves_partway_not_instantly() {
        let next = smooth_toward(0.0, 10.0, 0.5);
        assert!(next > 0.0 && next < 10.0);
    }

    #[test]
    fn smooth_toward_alpha_zero_snaps_immediately() {
        assert_eq!(smooth_toward(0.0, 10.0, 0.0), 10.0);
    }

    #[test]
    fn smooth_toward_alpha_one_stays_put() {
        assert_eq!(smooth_toward(3.0, 10.0, 1.0), 3.0);
    }

    #[test]
    fn one_pole_alpha_is_between_zero_and_one_for_positive_tau() {
        let a = one_pole_alpha(1.0, 44_100, 1024);
        assert!(a > 0.0 && a < 1.0);
    }

    #[test]
    fn one_pole_alpha_smaller_tau_yields_smaller_alpha() {
        // A shorter time constant should adapt faster, i.e. retain less of
        // the old value per step -- smaller alpha.
        let fast = one_pole_alpha(0.01, 44_100, 1024);
        let slow = one_pole_alpha(1.0, 44_100, 1024);
        assert!(fast < slow);
    }

    #[test]
    fn compressor_gain_is_unity_below_threshold() {
        assert_eq!(compressor_gain(0.5), 1.0);
        assert_eq!(compressor_gain(1.0), 1.0);
    }

    #[test]
    fn compressor_gain_reduces_above_threshold() {
        let g = compressor_gain(2.0);
        assert!((g - 0.5).abs() < 1e-9);
    }

    // --- NrjProcessor integration-style tests (synthetic PCM, no I/O) --

    fn constant_amplitude_chunk(amplitude: f32, frames: usize) -> Vec<f32> {
        let mut v = Vec::with_capacity(frames * 2);
        for _ in 0..frames {
            v.push(amplitude);
            v.push(-amplitude);
        }
        v
    }

    #[test]
    fn nrj_processor_leaves_silence_untouched() {
        let mut proc = NrjProcessor::new(44_100);
        let mut chunk = vec![0.0f32; 2048];
        proc.process(&mut chunk);
        assert!(chunk.iter().all(|s| *s == 0.0));
    }

    #[test]
    fn nrj_processor_leaves_a_signal_exactly_at_target_untouched() {
        // A signal exactly at 0dBFS RMS with target=0dB/gain_max=0dB is
        // already at the ceiling with no headroom to give up or reclaim --
        // both the normalizer (would-be gain = 0dB, i.e. no change) and the
        // compressor (only reduces once the envelope exceeds unity, which
        // an exactly-1.0 peak approaches but never crosses) should leave it
        // alone. This pins down that "at the ceiling" isn't itself treated
        // as an overshoot.
        let mut proc = NrjProcessor::new(44_100);
        let mut last_peak = 0.0f32;
        for _ in 0..200 {
            let mut chunk = constant_amplitude_chunk(1.0, 512);
            proc.process(&mut chunk);
            last_peak = chunk.iter().fold(0.0f32, |acc, s| acc.max(s.abs()));
        }
        assert!(
            (last_peak - 1.0).abs() < 1e-4,
            "expected a signal already at the target ceiling to be left alone, got peak {last_peak}"
        );
    }

    #[test]
    fn nrj_processor_attenuates_a_hot_sustained_signal_over_time() {
        // A signal sustained *above* full scale (a realistic "hot"/
        // inter-sample-peak master) should converge toward the compressor
        // pulling its peak back down, since its envelope will climb past
        // the unity threshold `compressor_gain` reduces from.
        let mut proc = NrjProcessor::new(44_100);
        let mut last_peak = 1.5f32;
        for _ in 0..200 {
            let mut chunk = constant_amplitude_chunk(1.5, 512);
            proc.process(&mut chunk);
            last_peak = chunk.iter().fold(0.0f32, |acc, s| acc.max(s.abs()));
        }
        assert!(
            last_peak < 1.5,
            "expected sustained hot input to be attenuated, got peak {last_peak}"
        );
    }

    #[test]
    fn nrj_processor_empty_chunk_is_a_no_op() {
        let mut proc = NrjProcessor::new(44_100);
        let mut chunk: Vec<f32> = vec![];
        proc.process(&mut chunk);
        assert!(chunk.is_empty());
    }

    // --- StereoToolProcessor byte marshalling (pure, no subprocess) ----
    //
    // The subprocess pipe itself (spawn/write/read against a real
    // `stereo_tool` binary) isn't exercised by any test here -- there's no
    // generic identity-pipe binary whose CLI grammar matches the fixed
    // `--silent - - -s <preset>` argument list `spawn` always passes (e.g.
    // `cat` would just error on those flags), so faking that integration
    // with a substitute binary would test the wrong thing. What's
    // unit-testable without spawning anything is the byte marshalling on
    // either side of the pipe, below. The real pipe needs a real binary --
    // see this module's doc for why that has to happen during jail
    // integration testing.

    #[test]
    fn samples_to_le_bytes_round_trips_through_le_bytes_into_samples() {
        let original = vec![0.25f32, -0.5, 0.75, -1.0, 0.0];
        let bytes = samples_to_le_bytes(&original);
        assert_eq!(bytes.len(), original.len() * 4);

        let mut decoded = vec![0.0f32; original.len()];
        le_bytes_into_samples(&bytes, &mut decoded);
        assert_eq!(decoded, original);
    }

    #[test]
    fn samples_to_le_bytes_matches_manual_encoding_for_one_sample() {
        let bytes = samples_to_le_bytes(&[1.0f32]);
        assert_eq!(bytes, 1.0f32.to_le_bytes().to_vec());
    }
}
