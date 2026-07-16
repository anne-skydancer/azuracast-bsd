//! Full in-memory audio decode: given a local file path, decode to
//! interleaved `f32` PCM and resample/remix to the engine's fixed internal
//! pipeline format (44100 Hz, stereo).
//!
//! Uses `symphonia` for container/codec decode (pure Rust, no system codec
//! libraries needed) and `rubato` for sample-rate conversion. Full-buffer
//! decode (the whole file is materialized in memory at once) is a
//! deliberate simplification for this phase — streaming/chunked decode is
//! an optimization, not a correctness requirement, per the task scope.

use std::fs::File;
use std::path::Path;

use rubato::{Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction};
use symphonia::core::audio::{AudioBufferRef, SampleBuffer};
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::{MetadataOptions, StandardTagKey, Value as TagValue};
use symphonia::core::probe::Hint;

/// The engine's fixed internal working sample rate. All decoded/mixed audio
/// is resampled to this rate so crossfade math never has to reason about
/// mismatched rates between the outgoing and incoming track.
pub const PIPELINE_SAMPLE_RATE: u32 = 44100;
/// The engine's fixed internal working channel count (stereo).
pub const PIPELINE_CHANNELS: u16 = 2;

/// A fully-decoded, resampled/remixed track ready for crossfade/playback.
#[derive(Debug, Clone)]
pub struct DecodedTrack {
    /// Interleaved PCM at `PIPELINE_SAMPLE_RATE` / `PIPELINE_CHANNELS`.
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub channels: u16,
    /// `REPLAYGAIN_TRACK_GAIN`-equivalent tag read from the file's own
    /// metadata, in dB, if the container/codec exposed one. Annotation-level
    /// replaygain (if PHP ever starts sending it) takes precedence over
    /// this when both are present -- see `prepare.rs`.
    pub replaygain_track_gain_db: Option<f64>,
}

impl DecodedTrack {
    pub fn frames(&self) -> usize {
        self.samples.len() / self.channels as usize
    }

    pub fn duration_secs(&self) -> f64 {
        self.frames() as f64 / self.sample_rate as f64
    }
}

/// Decodes `path` to `DecodedTrack`, fully resampled/remixed to
/// `PIPELINE_SAMPLE_RATE`/`PIPELINE_CHANNELS`. Errors are returned as
/// human-readable strings (consistent with the rest of this codebase's
/// `Result<_, String>` error style rather than a dedicated error enum).
pub fn decode_to_pcm(path: &Path) -> Result<DecodedTrack, String> {
    let file = File::open(path).map_err(|e| format!("failed to open {}: {e}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| format!("failed to probe {}: {e}", path.display()))?;
    let mut format = probed.format;

    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| format!("no decodable audio track found in {}", path.display()))?;
    let track_id = track.id;

    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| format!("failed to create decoder for {}: {e}", path.display()))?;

    let replaygain_track_gain_db = extract_replaygain_track_gain(format.as_mut());

    let mut interleaved: Vec<f32> = Vec::new();
    let mut source_rate: u32 = 0;
    let mut source_channels: usize = 0;

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymphoniaError::IoError(ref e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break
            }
            Err(SymphoniaError::ResetRequired) => break,
            Err(e) => return Err(format!("error reading packet from {}: {e}", path.display())),
        };

        if packet.track_id() != track_id {
            continue;
        }

        match decoder.decode(&packet) {
            Ok(decoded) => {
                let spec = *decoded.spec();
                if source_rate == 0 {
                    source_rate = spec.rate;
                    source_channels = spec.channels.count();
                }
                append_interleaved(&decoded, &mut interleaved);
            }
            // A single bad packet shouldn't kill the whole decode; skip it
            // and keep going, matching how most consumer decoders behave.
            Err(SymphoniaError::DecodeError(msg)) => {
                tracing::warn!("decode error in {} (skipping packet): {msg}", path.display());
                continue;
            }
            Err(e) => return Err(format!("fatal decode error in {}: {e}", path.display())),
        }
    }

    if source_rate == 0 || source_channels == 0 {
        return Err(format!("no audio frames decoded from {}", path.display()));
    }

    let planes = to_stereo_channel_planes(&interleaved, source_channels);
    let resampled_planes = resample_channels(planes, source_rate, PIPELINE_SAMPLE_RATE)?;
    let samples = interleave_stereo(&resampled_planes);

    Ok(DecodedTrack {
        samples,
        sample_rate: PIPELINE_SAMPLE_RATE,
        channels: PIPELINE_CHANNELS,
        replaygain_track_gain_db,
    })
}

/// Copies one decoded audio buffer (in whatever the source's native format
/// is) into the running interleaved-f32 accumulator. `pub(crate)`: shared
/// with `harbor.rs`'s streaming live-decode path (Phase 4).
pub(crate) fn append_interleaved(decoded: &AudioBufferRef, out: &mut Vec<f32>) {
    let spec = *decoded.spec();
    let channels = spec.channels.count();
    let frames = decoded.frames();
    // Zero-frame decoded buffers are real -- Vorbis emits one for its
    // first/priming packet -- and symphonia-core 0.5.5's
    // SampleBuffer::copy_interleaved_ref PANICS on them ("range start
    // index 1 out of range for slice of length 0", audio.rs:856).
    // Confirmed on a real install: without this guard the engine
    // crash-looped on the first packet of every .ogg track. Nothing to
    // copy anyway.
    if frames == 0 || channels == 0 {
        return;
    }
    let mut sample_buf = SampleBuffer::<f32>::new(frames as u64, spec);
    sample_buf.copy_interleaved_ref(decoded.clone());
    out.reserve(frames * channels);
    out.extend_from_slice(sample_buf.samples());
}

/// Looks for a `REPLAYGAIN_TRACK_GAIN`-style standard tag in the format's
/// metadata log (checks both the container-level log and whatever was
/// picked up during probing) and parses its leading numeric dB value.
fn extract_replaygain_track_gain(
    format: &mut dyn symphonia::core::formats::FormatReader,
) -> Option<f64> {
    let metadata = format.metadata();
    let revision = metadata.current()?;
    for tag in revision.tags() {
        if tag.std_key == Some(StandardTagKey::ReplayGainTrackGain) {
            let text = match &tag.value {
                TagValue::String(s) => s.clone(),
                other => format!("{other}"),
            };
            return parse_leading_float(&text);
        }
    }
    None
}

/// Parses the leading floating-point number out of a string like
/// `"-3.2 dB"` or `"3.2"`, returning `None` if no number is found at the
/// start. Shared by replaygain-tag parsing (here) and `liq_amplify`
/// annotation parsing (`prepare.rs`), since both use the same `"{n} dB"`
/// shape.
pub fn parse_leading_float(s: &str) -> Option<f64> {
    let s = s.trim();
    let bytes = s.as_bytes();
    let mut seen_digit = false;
    let mut seen_dot = false;
    let mut i = 0usize;
    if i < bytes.len() && (bytes[i] == b'-' || bytes[i] == b'+') {
        i += 1;
    }
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c.is_ascii_digit() {
            seen_digit = true;
            i += 1;
        } else if c == '.' && !seen_dot {
            seen_dot = true;
            i += 1;
        } else {
            break;
        }
    }
    let end = i;
    if !seen_digit {
        return None;
    }
    s[..end].parse::<f64>().ok()
}

/// Splits interleaved source PCM into per-channel planes, remixed to
/// exactly 2 channels (stereo):
/// - mono (1 channel): duplicated to L/R.
/// - stereo (2 channels): passed through as-is.
/// - anything else (>2 channels): averaged down to mono first, then
///   duplicated to L/R. This is a simpler-than-ideal downmix (no proper
///   center/surround mix matrix), a documented scope simplification since
///   the task only calls out mono<->stereo handling explicitly.
pub(crate) fn to_stereo_channel_planes(interleaved: &[f32], channels: usize) -> Vec<Vec<f32>> {
    let frames = interleaved.len() / channels.max(1);

    if channels == 2 {
        let mut l = Vec::with_capacity(frames);
        let mut r = Vec::with_capacity(frames);
        for f in 0..frames {
            l.push(interleaved[f * 2]);
            r.push(interleaved[f * 2 + 1]);
        }
        return vec![l, r];
    }

    if channels == 1 {
        let mono: Vec<f32> = interleaved.to_vec();
        return vec![mono.clone(), mono];
    }

    let mut mono = Vec::with_capacity(frames);
    for f in 0..frames {
        let mut sum = 0.0f32;
        for c in 0..channels {
            sum += interleaved[f * channels + c];
        }
        mono.push(sum / channels as f32);
    }
    vec![mono.clone(), mono]
}

pub(crate) fn interleave_stereo(planes: &[Vec<f32>]) -> Vec<f32> {
    let frames = planes[0].len();
    let mut out = Vec::with_capacity(frames * 2);
    for f in 0..frames {
        out.push(planes[0][f]);
        out.push(planes[1][f]);
    }
    out
}

/// Resamples each channel plane from `source_rate` to `target_rate` using
/// `rubato`'s sinc-interpolation fixed-input resampler. Returns the planes
/// unchanged if the rates already match.
fn resample_channels(
    channels_data: Vec<Vec<f32>>,
    source_rate: u32,
    target_rate: u32,
) -> Result<Vec<Vec<f32>>, String> {
    if source_rate == target_rate {
        return Ok(channels_data);
    }
    if channels_data.is_empty() || channels_data[0].is_empty() {
        return Ok(channels_data);
    }

    let ratio = target_rate as f64 / source_rate as f64;
    let params = SincInterpolationParameters {
        sinc_len: 256,
        f_cutoff: 0.95,
        interpolation: SincInterpolationType::Linear,
        oversampling_factor: 256,
        window: WindowFunction::BlackmanHarris2,
    };
    let chunk_size = 1024usize;
    let num_channels = channels_data.len();

    let mut resampler = SincFixedIn::<f32>::new(ratio, 2.0, params, chunk_size, num_channels)
        .map_err(|e| format!("failed to create resampler: {e}"))?;

    let total_in_frames = channels_data[0].len();
    let mut output: Vec<Vec<f32>> = vec![Vec::new(); num_channels];
    let mut pos = 0usize;

    while pos < total_in_frames {
        let end = (pos + chunk_size).min(total_in_frames);
        let input_chunk: Vec<Vec<f32>> = channels_data
            .iter()
            .map(|ch| ch[pos..end].to_vec())
            .collect();

        let out_chunk = resampler
            .process_partial(Some(&input_chunk), None)
            .map_err(|e| format!("resample error: {e}"))?;
        for (ch_out, ch_new) in output.iter_mut().zip(out_chunk.into_iter()) {
            ch_out.extend(ch_new);
        }
        pos = end;
    }

    // Flush any samples still buffered inside the resampler's internal
    // delay line.
    let flush = resampler
        .process_partial::<Vec<f32>>(None, None)
        .map_err(|e| format!("resample flush error: {e}"))?;
    for (ch_out, ch_new) in output.iter_mut().zip(flush.into_iter()) {
        ch_out.extend(ch_new);
    }

    Ok(output)
}

/// Incremental (streaming) counterpart to `resample_channels`: wraps a
/// single `rubato` resampler instance that persists across many small
/// `process` calls instead of one whole-file batch loop, so its internal
/// delay line/history carries over correctly between chunks arriving at
/// unpredictable sizes over time -- exactly `harbor.rs`'s live-decode
/// use case (Phase 4), where a "chunk" is whatever one decoded symphonia
/// packet produced, not a fixed-size window. Not used by `decode_to_pcm`'s
/// existing full-buffer path, which keeps its own batch-loop resampler
/// local to `resample_channels` unchanged.
pub struct StreamResampler {
    resampler: Option<SincFixedIn<f32>>,
}

impl StreamResampler {
    /// `channels` is always `PIPELINE_CHANNELS` in practice (harbor audio is
    /// remixed to stereo before reaching this point, same as the full-buffer
    /// path), but taken as a parameter rather than hardcoded for clarity at
    /// the call site.
    pub fn new(source_rate: u32, target_rate: u32, channels: usize) -> Result<Self, String> {
        if source_rate == target_rate {
            return Ok(Self { resampler: None });
        }
        let ratio = target_rate as f64 / source_rate as f64;
        let params = SincInterpolationParameters {
            sinc_len: 256,
            f_cutoff: 0.95,
            interpolation: SincInterpolationType::Linear,
            oversampling_factor: 256,
            window: WindowFunction::BlackmanHarris2,
        };
        // Chunk size here is just rubato's internal processing granularity
        // hint, not a hard requirement on caller input size -- `process`
        // below always goes through `process_partial`, which explicitly
        // supports irregularly-sized inputs (see `resample_channels`'s own
        // use of the same method for the equivalent reason).
        let resampler = SincFixedIn::<f32>::new(ratio, 2.0, params, 1024, channels)
            .map_err(|e| format!("failed to create streaming resampler: {e}"))?;
        Ok(Self { resampler: Some(resampler) })
    }

    /// Resamples one chunk of per-channel planes (arbitrary, non-fixed
    /// length), returning however many resampled frames that produced
    /// (possibly zero, if not enough input has accumulated yet to satisfy
    /// the resampler's internal windowing -- callers should just skip
    /// forwarding empty results, not treat them as an error).
    pub fn process(&mut self, planes: Vec<Vec<f32>>) -> Result<Vec<Vec<f32>>, String> {
        match self.resampler.as_mut() {
            None => Ok(planes),
            Some(resampler) => resampler
                .process_partial(Some(&planes), None)
                .map_err(|e| format!("streaming resample error: {e}")),
        }
    }
}
