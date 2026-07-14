//! AutoDJ next-track resolution with SPEC.md C.3's retry semantics, and the
//! priority-queue-vs-AutoDJ dispatch from SPEC.md C.8 (scope-restricted per
//! the task: no live harbor, no remote-URL fallback, no schedule switches
//! -- those are either a later phase or entirely PHP's job already).

use std::path::Path;
use std::time::Duration;

use crate::annotate::parse_annotated_uri;
use crate::callbacks::CallbackClient;
use crate::decode::decode_to_pcm;
use crate::media::resolve_media;
use crate::prepare::{prepare_fallback_track, prepare_track, PreparedTrack};
use crate::queue::TrackQueues;

/// SPEC.md C.3: `retry_delay=10.` -- fixed, not station-configurable.
pub const RETRY_DELAY: Duration = Duration::from_secs(10);

/// Resolves the next track to play, in priority order: `interrupting_requests`
/// > `requests` > AutoDJ (`nextsong`). Never returns without a playable
/// track: on repeated AutoDJ failure it loops the configured fallback file
/// (SPEC.md C.8's `error_jingle`) if one is set, or produces a
/// `RETRY_DELAY`-long burst of silence (and genuinely waits out that delay)
/// if not, so the pipeline never crashes or stalls even with PHP
/// unreachable and no fallback configured.
pub async fn fetch_next_track(
    client: &CallbackClient,
    queues: &TrackQueues,
    replaygain_enabled: bool,
    fallback_file_path: Option<&str>,
) -> PreparedTrack {
    loop {
        if let Some(uri) = queues.pop_next() {
            match resolve_and_prepare(client, &uri, replaygain_enabled).await {
                Ok(track) => return track,
                Err(e) => {
                    tracing::warn!("failed to prepare queued track '{uri}' (skipping): {e}");
                    continue;
                }
            }
        }

        match client.call_nextsong().await {
            Ok(resp) => match resolve_and_prepare(client, &resp.uri, replaygain_enabled).await {
                Ok(track) => return track,
                Err(e) => {
                    tracing::warn!(
                        "failed to prepare AutoDJ track '{}' (falling back): {e}",
                        resp.uri
                    );
                }
            },
            Err(e) => {
                tracing::warn!("nextsong callback failed: {e}");
            }
        }

        // Nothing playable came back this attempt. Per SPEC.md C.8 (this
        // task's simplified fallback chain), prefer looping the configured
        // fallback/error file over going silent indefinitely.
        if let Some(path) = fallback_file_path {
            match decode_to_pcm(Path::new(path)) {
                Ok(decoded) => return prepare_fallback_track(decoded),
                Err(e) => {
                    tracing::error!("failed to decode fallback file '{path}': {e}");
                }
            }
        }

        // No fallback file (or it failed to decode too): genuinely wait
        // out the retry delay before the caller asks again, and hand back
        // silence to fill that gap -- this is what actually rate-limits
        // repeated `nextsong` calls, since this pipeline isn't wall-clock
        // paced by a real-time output device (see `pipeline.rs`).
        tracing::warn!(
            "AutoDJ has nothing to play; waiting {:?} before retrying (playing silence)",
            RETRY_DELAY
        );
        tokio::time::sleep(RETRY_DELAY).await;
        return silence_track(RETRY_DELAY);
    }
}

async fn resolve_and_prepare(
    client: &CallbackClient,
    uri: &str,
    replaygain_enabled: bool,
) -> Result<PreparedTrack, String> {
    let annotations = parse_annotated_uri(uri);
    if annotations.is_empty() {
        tracing::debug!("resolving unannotated track: {}", annotations.path);
    } else {
        tracing::debug!(
            "resolving track with {} annotation(s): {}",
            annotations.len(),
            annotations.path
        );
    }
    let resolved = resolve_media(client, &annotations.path).await?;
    let decode_result = decode_to_pcm(&resolved.local_path);
    resolved.cleanup();
    let decoded = decode_result?;
    Ok(prepare_track(decoded, &annotations, replaygain_enabled))
}

/// A silence-filled `PreparedTrack` of the given duration. Tagged
/// `is_error_file` so it's suppressed from `feedback` like any other filler
/// audio (SPEC.md C.6/C.8).
fn silence_track(duration: Duration) -> PreparedTrack {
    use crate::decode::{DecodedTrack, PIPELINE_CHANNELS, PIPELINE_SAMPLE_RATE};

    let frames = (duration.as_secs_f64() * PIPELINE_SAMPLE_RATE as f64).round() as usize;
    let decoded = DecodedTrack {
        samples: vec![0.0f32; frames * PIPELINE_CHANNELS as usize],
        sample_rate: PIPELINE_SAMPLE_RATE,
        channels: PIPELINE_CHANNELS,
        replaygain_track_gain_db: None,
    };
    prepare_fallback_track(decoded)
}
