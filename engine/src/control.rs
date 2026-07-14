//! Shared control-plane signals between the axum control API (`server.rs`)
//! and the playback pipeline loop (`pipeline.rs`), backing the `/skip` and
//! `/metadata` routes -- SPEC.md C.9's `add_skip_command` (`source.skip(s)`)
//! and `add_custom_metadata_command` (`custom_metadata.insert`).
//!
//! **Why polling, not a blocking wait:** `pipeline.rs`'s loop has no
//! real-time output pacing yet (see its module doc) -- it's a straight-line
//! loop that is never otherwise blocked waiting on external input, running
//! as fast as decode/crossfade can produce output. A blocking primitive
//! like `tokio::sync::Notify::notified().await` would stall that loop
//! forever between skips/metadata pushes, which is exactly backwards. So
//! both signals here are simple state the axum handlers set
//! (fire-and-forget, non-blocking) and `pipeline.rs` polls once per loop
//! iteration (non-blocking) -- matching the existing buffer-position-driven
//! design instead of fighting it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// Shared handle: one instance is constructed in `main.rs`, cloned (via
/// `Arc`) into both `server::AppState` (the axum-handler side, which only
/// ever writes) and `pipeline::Pipeline` (the loop side, which only ever
/// reads/consumes).
#[derive(Debug, Default)]
pub struct ControlSignals {
    skip_requested: AtomicBool,
    metadata_override: Mutex<HashMap<String, String>>,
}

impl ControlSignals {
    pub fn new() -> Self {
        Self::default()
    }

    /// Called from the `/skip` axum handler. Fire-and-forget: flips a flag
    /// for the pipeline loop to notice on its next iteration; does not wait
    /// for the pipeline to actually act on it.
    pub fn request_skip(&self) {
        self.skip_requested.store(true, Ordering::SeqCst);
    }

    /// Called from the `/metadata` axum handler. Merges `meta` into
    /// whatever override is already pending (new values win per key),
    /// mirroring `custom_metadata.insert`'s "insert" semantics -- if two
    /// `/metadata` calls land before the pipeline catches up, they
    /// accumulate rather than the second clobbering the first entirely.
    pub fn set_metadata_override(&self, meta: HashMap<String, String>) {
        let mut pending = self.metadata_override.lock().unwrap();
        for (k, v) in meta {
            pending.insert(k, v);
        }
    }

    /// Consumes and resets the skip flag. One-shot: "was a skip requested
    /// since I last checked". Only `pipeline.rs`'s loop should call this.
    pub fn take_skip(&self) -> bool {
        self.skip_requested.swap(false, Ordering::SeqCst)
    }

    /// Consumes and clears any pending metadata override, returning `None`
    /// if nothing is pending. Only `pipeline.rs`'s loop should call this.
    pub fn take_metadata_override(&self) -> Option<HashMap<String, String>> {
        let mut pending = self.metadata_override.lock().unwrap();
        if pending.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut *pending))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skip_round_trips_once() {
        let signals = ControlSignals::new();
        assert!(!signals.take_skip());
        signals.request_skip();
        assert!(signals.take_skip());
        // One-shot: a second check without a new request sees nothing.
        assert!(!signals.take_skip());
    }

    #[test]
    fn metadata_override_merges_and_is_consumed_once() {
        let signals = ControlSignals::new();
        assert!(signals.take_metadata_override().is_none());

        let mut first = HashMap::new();
        first.insert("title".to_string(), "A".to_string());
        signals.set_metadata_override(first);

        let mut second = HashMap::new();
        second.insert("artist".to_string(), "B".to_string());
        second.insert("title".to_string(), "C".to_string()); // overwrites "A"
        signals.set_metadata_override(second);

        let pending = signals.take_metadata_override().unwrap();
        assert_eq!(pending.get("title"), Some(&"C".to_string()));
        assert_eq!(pending.get("artist"), Some(&"B".to_string()));

        // Consumed: nothing left pending.
        assert!(signals.take_metadata_override().is_none());
    }
}
