//! Autocue (branch 1 only) + `liq_amplify` + replaygain application, per
//! `engine/SPEC.md` C.10/C.11 and D.1's `processAutocueAnnotations`.
//!
//! **Autocue branch 2** (on-the-fly cue-point computation via internal
//! loudness analysis + a `savecache` round trip) is explicitly out of scope
//! for this phase -- see the task notes and the final report. Branch 3 (no
//! autocue data at all) needs no special handling here: if
//! `azuracast_autocue` isn't `"true"`, the track is simply left untrimmed.

use std::collections::HashMap;

use crate::annotate::Annotations;
use crate::decode::{parse_leading_float, DecodedTrack};

/// The subset of annotation-derived metadata `feedback` (SPEC.md C.6) and
/// jingle-mode suppression (C.7 #2) care about.
#[derive(Debug, Clone, Default)]
pub struct TrackMetadata {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub song_id: Option<String>,
    pub media_id: Option<String>,
    pub sq_id: Option<String>,
    pub playlist_id: Option<String>,
    /// The media row's unique id (PHP-side `StationMedia::unique_id`),
    /// used to build the track's album-art URL for in-band art embedding
    /// (see `pipeline.rs::publish_now_playing` / `output.rs`). Not part
    /// of the `feedback` payload.
    pub media_unique_id: Option<String>,
}

impl TrackMetadata {
    pub fn from_annotations(a: &Annotations) -> Self {
        Self {
            title: a.get("title").map(|s| s.to_string()),
            artist: a.get("artist").map(|s| s.to_string()),
            song_id: a.get("song_id").map(|s| s.to_string()),
            media_id: a.get("media_id").map(|s| s.to_string()),
            sq_id: a.get("sq_id").map(|s| s.to_string()),
            playlist_id: a.get("playlist_id").map(|s| s.to_string()),
            media_unique_id: a.get("media_unique_id").map(|s| s.to_string()),
        }
    }

    /// `true` if there is no reportable metadata at all (title/artist both
    /// absent) -- used together with `jingle_mode`/`is_error_file` to decide
    /// whether `feedback` should fire (SPEC.md D.5 requires at least one of
    /// artist/title, or a `media_id`, to be meaningful).
    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.artist.is_none()
            && self.media_id.is_none()
            && self.song_id.is_none()
    }

    /// Applies a `/metadata` control-API override (SPEC.md C.9's
    /// `custom_metadata.insert key="val",...`, `insert_metadata` on the
    /// currently-playing source) onto this track's metadata. New values win
    /// over whatever was already set from `nextsong` annotations -- plain
    /// merge/overwrite, per key.
    ///
    /// Only the six fields `feedback`/C.6 actually forwards are recognized
    /// here (matching `FeedbackPayload`'s shape); unrecognized keys are
    /// ignored, mirroring the fact that `send_feedback` only ever forwards
    /// this filtered subset regardless of what other keys `on_metadata`
    /// carries.
    pub fn apply_overrides(&mut self, overrides: &HashMap<String, String>) {
        if let Some(v) = overrides.get("title") {
            self.title = Some(v.clone());
        }
        if let Some(v) = overrides.get("artist") {
            self.artist = Some(v.clone());
        }
        if let Some(v) = overrides.get("song_id") {
            self.song_id = Some(v.clone());
        }
        if let Some(v) = overrides.get("media_id") {
            self.media_id = Some(v.clone());
        }
        if let Some(v) = overrides.get("sq_id") {
            self.sq_id = Some(v.clone());
        }
        if let Some(v) = overrides.get("playlist_id") {
            self.playlist_id = Some(v.clone());
        }
    }
}

/// A decoded track with autocue trim/gain/fade overrides applied, ready to
/// be handed to the crossfade stage.
#[derive(Debug, Clone)]
pub struct PreparedTrack {
    pub decoded: DecodedTrack,
    pub metadata: TrackMetadata,
    /// SPEC.md C.7 #2: this track's metadata should not update
    /// `last_title`/`last_artist` nor trigger a `feedback` call at all.
    pub jingle_mode: bool,
    /// Set for the fallback/error-jingle file only (SPEC.md C.8's
    /// `is_error_file="true"` tag) -- also suppresses `feedback`, via the
    /// same mechanism as `jingle_mode` (see `feedback.rs`).
    pub is_error_file: bool,
    /// Overrides the station-default fade-in duration for the transition
    /// *into* this track, from `autocue_fade_in` (branch-1 autocue only).
    pub fade_in_override: Option<f64>,
    /// Overrides the station-default fade-out duration for the transition
    /// *out of* this track, from `autocue_fade_out`.
    pub fade_out_override: Option<f64>,
    /// `autocue_start_next`, re-based to seconds from the (already-trimmed)
    /// track start. Threaded through for a future lookahead scheduler to
    /// consume; this phase's simpler structural lookahead (see
    /// `pipeline.rs`'s module doc) does not yet act on it -- documented
    /// follow-up, not a bug.
    #[allow(dead_code)]
    pub start_next: Option<f64>,
    /// New in Phase 4: `true` for chunks produced by the live-DJ harbor
    /// input (`harbor.rs`) rather than AutoDJ/queued media. Drives
    /// `pipeline.rs`'s live-vs-AutoDJ transition handling (crossfade
    /// dispatch, windowing) -- see its module doc.
    pub is_live: bool,
}

/// Applies branch-1 autocue trim, `liq_amplify`, and (if enabled)
/// replaygain to a freshly-decoded track.
pub fn prepare_track(
    mut decoded: DecodedTrack,
    annotations: &Annotations,
    replaygain_enabled: bool,
) -> PreparedTrack {
    let channels = decoded.channels as usize;
    let sample_rate = decoded.sample_rate as f64;

    let mut fade_in_override = None;
    let mut fade_out_override = None;
    let mut start_next = None;

    if annotations.get_bool("azuracast_autocue") {
        let duration = decoded.duration_secs();
        let cue_in = annotations
            .get_f64("autocue_cue_in")
            .unwrap_or(0.0)
            .clamp(0.0, duration.max(0.0));
        let cue_out = annotations
            .get_f64("autocue_cue_out")
            .unwrap_or(duration)
            .min(duration)
            .max(cue_in);

        let start_frame = ((cue_in * sample_rate).round() as usize).min(decoded.frames());
        let end_frame = ((cue_out * sample_rate).round() as usize).min(decoded.frames());
        if end_frame > start_frame {
            let start_sample = start_frame * channels;
            let end_sample = end_frame * channels;
            decoded.samples = decoded.samples[start_sample..end_sample].to_vec();
        }

        fade_in_override = annotations.get_f64("autocue_fade_in");
        fade_out_override = annotations.get_f64("autocue_fade_out");
        start_next = annotations
            .get_f64("autocue_start_next")
            .map(|s| (s - cue_in).max(0.0));
    }

    if let Some(amplify_str) = annotations.get("liq_amplify") {
        if let Some(db) = parse_leading_float(amplify_str) {
            apply_linear_gain(&mut decoded.samples, db_to_linear(db));
        }
    }

    if replaygain_enabled {
        // Prefer an explicit annotation over the file's own tag, per the
        // task's stated preference order.
        let rg_db = annotations
            .get_f64("replaygain_track_gain")
            .or(decoded.replaygain_track_gain_db);
        if let Some(db) = rg_db {
            apply_linear_gain(&mut decoded.samples, db_to_linear(db));
        }
    }

    PreparedTrack {
        metadata: TrackMetadata::from_annotations(annotations),
        jingle_mode: annotations.get_bool("jingle_mode"),
        is_error_file: false,
        fade_in_override,
        fade_out_override,
        start_next,
        is_live: false,
        decoded,
    }
}

/// Wraps an already-decoded fallback/error file as a `PreparedTrack` tagged
/// so `feedback.rs` knows to suppress it, per SPEC.md C.8/C.6
/// (`is_error_file="true"`).
pub fn prepare_fallback_track(decoded: DecodedTrack) -> PreparedTrack {
    PreparedTrack {
        decoded,
        metadata: TrackMetadata::default(),
        jingle_mode: false,
        is_error_file: true,
        fade_in_override: None,
        fade_out_override: None,
        start_next: None,
        is_live: false,
    }
}

/// Wraps one decoded chunk of live-DJ harbor audio as a `PreparedTrack`
/// (Phase 4). Deliberately carries no metadata: SPEC.md B.4 point 3 notes
/// the `insert_missing` step that *would* attach `is_live`/`live_broadcast_text`
/// metadata is commented out in the real Liquidsoap config generation
/// ("Temporarily disabled for testing") -- i.e. this is a real, verified
/// no-op the engine must match, not an oversight. Empty metadata also means
/// `FeedbackDedup::maybe_send` naturally suppresses `feedback` for live
/// audio via its existing `metadata.is_empty()` guard, which is the correct
/// behavior for a station whose source client isn't sending mid-stream ICY
/// metadata (parsing that is explicitly deferred -- see `harbor.rs`).
pub fn prepare_live_chunk(decoded: DecodedTrack) -> PreparedTrack {
    PreparedTrack {
        decoded,
        metadata: TrackMetadata::default(),
        jingle_mode: false,
        is_error_file: false,
        fade_in_override: None,
        fade_out_override: None,
        start_next: None,
        is_live: true,
    }
}

/// `10^(db/20)`, i.e. plain linear-gain conversion -- SPEC.md C.11's
/// `amplify()` semantics, a straight multiply with no dynamics/compression.
pub fn db_to_linear(db: f64) -> f32 {
    10f64.powf(db / 20.0) as f32
}

fn apply_linear_gain(samples: &mut [f32], gain: f32) {
    for s in samples.iter_mut() {
        *s *= gain;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_overrides_wins_over_existing_values() {
        let mut meta = TrackMetadata {
            title: Some("Old Title".to_string()),
            artist: Some("Old Artist".to_string()),
            media_id: Some("media-1".to_string()),
            ..Default::default()
        };

        let mut overrides = HashMap::new();
        overrides.insert("title".to_string(), "New Title".to_string());
        overrides.insert("song_id".to_string(), "song-42".to_string());
        overrides.insert("unknown_key".to_string(), "ignored".to_string());

        meta.apply_overrides(&overrides);

        assert_eq!(meta.title, Some("New Title".to_string()));
        assert_eq!(meta.artist, Some("Old Artist".to_string()));
        assert_eq!(meta.song_id, Some("song-42".to_string()));
        assert_eq!(meta.media_id, Some("media-1".to_string()));
    }

    #[test]
    fn apply_overrides_empty_map_is_noop() {
        let mut meta = TrackMetadata {
            title: Some("Title".to_string()),
            ..Default::default()
        };
        meta.apply_overrides(&HashMap::new());
        assert_eq!(meta.title, Some("Title".to_string()));
    }
}
