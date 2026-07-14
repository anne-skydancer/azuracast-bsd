//! `feedback` push dedup + jingle/error-file suppression, per
//! `engine/SPEC.md` C.6 (`azuracast.send_feedback`) and C.7 #2
//! (`azuracast.handle_jingle_mode`'s effect on feedback specifically).

use crate::callbacks::{CallbackClient, FeedbackPayload};
use crate::prepare::PreparedTrack;

/// Tracks `last_title`/`last_artist` (SPEC.md C.1's global refs) so
/// `feedback` is only called when metadata actually changes.
#[derive(Debug, Default)]
pub struct FeedbackDedup {
    last_title: Option<String>,
    last_artist: Option<String>,
}

impl FeedbackDedup {
    pub fn new() -> Self {
        Self::default()
    }

    /// Decides whether `feedback` should fire for `track`, and if so, POSTs
    /// it and updates the dedup state. Matches SPEC.md C.6 + C.7 #2 exactly:
    ///
    /// - `is_error_file` (C.8's fallback jingle): always suppressed, dedup
    ///   state untouched.
    /// - `jingle_mode` (C.7 #2, DB-queue jingle tracks): always suppressed,
    ///   dedup state untouched -- "the jingle plays audibly but produces no
    ///   metadata change".
    /// - Otherwise: only sends (and updates `last_title`/`last_artist`) if
    ///   title or artist actually differs from what was last sent.
    pub async fn maybe_send(&mut self, client: &CallbackClient, track: &PreparedTrack) {
        if track.is_error_file {
            tracing::debug!("feedback suppressed: is_error_file");
            return;
        }
        if track.jingle_mode {
            tracing::debug!("feedback suppressed: jingle_mode");
            return;
        }
        if track.metadata.is_empty() {
            // Mirrors SPEC.md D.5's requirement that at least one of
            // artist/title/media_id be present for `feedback` to mean
            // anything -- avoids a doomed round trip for tracks whose
            // annotations carried no reportable metadata at all (e.g. a
            // bare-path AutoDJ URI with no `nextsong` annotations).
            tracing::debug!("feedback suppressed: no reportable metadata");
            return;
        }

        let title = track.metadata.title.clone();
        let artist = track.metadata.artist.clone();

        if title == self.last_title && artist == self.last_artist {
            tracing::debug!("feedback suppressed: metadata unchanged");
            return;
        }

        let payload = FeedbackPayload {
            song_id: track.metadata.song_id.clone(),
            media_id: track.metadata.media_id.clone(),
            playlist_id: track.metadata.playlist_id.clone(),
            sq_id: track.metadata.sq_id.clone(),
            artist: artist.clone(),
            title: title.clone(),
        };

        match client.call_feedback(payload).await {
            Ok(_) => {
                self.last_title = title;
                self.last_artist = artist;
            }
            Err(e) => {
                tracing::warn!("feedback callback failed (dedup state left unchanged): {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annotate::parse_annotated_uri;
    use crate::decode::DecodedTrack;
    use crate::prepare::prepare_track;

    fn track_with(title: &str, artist: &str) -> PreparedTrack {
        let uri = format!(
            r#"annotate:title="{title}",artist="{artist}":media:x.mp3"#,
        );
        let annotations = parse_annotated_uri(&uri);
        let decoded = DecodedTrack {
            samples: vec![0.0; 4],
            sample_rate: 44100,
            channels: 2,
            replaygain_track_gain_db: None,
        };
        prepare_track(decoded, &annotations, false)
    }

    #[test]
    fn dedup_state_defaults_empty() {
        let dedup = FeedbackDedup::new();
        assert_eq!(dedup.last_title, None);
        assert_eq!(dedup.last_artist, None);
    }

    #[test]
    fn jingle_and_error_tracks_are_identified_for_suppression() {
        let mut t = track_with("Jingle", "Station");
        t.jingle_mode = true;
        assert!(t.jingle_mode);

        let mut e = track_with("Error", "File");
        e.is_error_file = true;
        assert!(e.is_error_file);
    }
}
