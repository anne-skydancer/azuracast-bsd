//! In-memory priority request queues, matching PHP's `LiquidsoapQueues`
//! enum values (`"requests"`, `"interrupting_requests"`) -- SPEC.md B.2's
//! `requests`/`interrupting_queue` `request.queue()` sources.
//!
//! No shared type is needed across the FFI/HTTP boundary here: the control
//! API (`server.rs`) already receives/logs the queue name as a bare string,
//! so this module just validates against the two known names.

use std::collections::VecDeque;
use std::sync::Mutex;

pub const QUEUE_REQUESTS: &str = "requests";
pub const QUEUE_INTERRUPTING: &str = "interrupting_requests";

/// The two AutoDJ priority queues. Thread-safe via `Mutex` since both the
/// control API (push/empty-check handlers) and the playback loop
/// (pop-next) touch these concurrently.
#[derive(Debug, Default)]
pub struct TrackQueues {
    requests: Mutex<VecDeque<String>>,
    interrupting_requests: Mutex<VecDeque<String>>,
}

impl TrackQueues {
    pub fn new() -> Self {
        Self::default()
    }

    /// Pushes `uri` onto the named queue. Returns `Err` for any queue name
    /// other than the two known ones, so the control API can surface a
    /// clear error instead of silently dropping the request.
    pub fn push(&self, queue: &str, uri: String) -> Result<(), String> {
        match queue {
            QUEUE_REQUESTS => {
                self.requests.lock().unwrap().push_back(uri);
                Ok(())
            }
            QUEUE_INTERRUPTING => {
                self.interrupting_requests.lock().unwrap().push_back(uri);
                Ok(())
            }
            other => Err(format!("unknown queue '{other}'")),
        }
    }

    /// `true` if the named queue has nothing pending. Unknown queue names
    /// are reported empty (mirrors the Phase 2 handler's permissive
    /// behavior -- the control API doesn't hard-fail on an unrecognized
    /// queue name, it just has nothing to report).
    pub fn is_empty(&self, queue: &str) -> bool {
        match queue {
            QUEUE_REQUESTS => self.requests.lock().unwrap().is_empty(),
            QUEUE_INTERRUPTING => self.interrupting_requests.lock().unwrap().is_empty(),
            _ => true,
        }
    }

    /// Pops the next URI to play, respecting SPEC.md C.8's priority order
    /// (restricted to what's in scope for this task -- no live harbor, no
    /// remote-URL fallback, no schedule switches): `interrupting_requests`
    /// (if non-empty) outranks `requests`. Returns `None` if both queues
    /// are empty, meaning the caller should fall through to AutoDJ.
    ///
    /// Phase 4 note: the live-DJ harbor slots in *between* these two
    /// queues in the real priority order (`interrupting_requests` > live >
    /// `requests` > AutoDJ -- SPEC.md C.8), which this single combined
    /// method can't express on its own; `autodj::fetch_next_track` uses
    /// `pop_interrupting`/`pop_requests` directly (with a live check
    /// in between) instead of this method for that reason. `pop_next` is
    /// kept as-is (and still unit-tested below) for any caller that only
    /// cares about the two request queues' own relative order.
    pub fn pop_next(&self) -> Option<String> {
        if let Some(uri) = self.pop_interrupting() {
            return Some(uri);
        }
        self.pop_requests()
    }

    /// Pops the next URI from `interrupting_requests` only (SPEC.md C.8's
    /// highest-priority queue, outranking even the live-DJ harbor).
    pub fn pop_interrupting(&self) -> Option<String> {
        self.interrupting_requests.lock().unwrap().pop_front()
    }

    /// Pops the next URI from `requests` only (SPEC.md C.8's queue ranked
    /// just below the live-DJ harbor, above AutoDJ).
    pub fn pop_requests(&self) -> Option<String> {
        self.requests.lock().unwrap().pop_front()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupting_beats_requests() {
        let q = TrackQueues::new();
        q.push(QUEUE_REQUESTS, "a.mp3".to_string()).unwrap();
        q.push(QUEUE_INTERRUPTING, "b.mp3".to_string()).unwrap();
        assert_eq!(q.pop_next(), Some("b.mp3".to_string()));
        assert_eq!(q.pop_next(), Some("a.mp3".to_string()));
        assert_eq!(q.pop_next(), None);
    }

    #[test]
    fn unknown_queue_rejected() {
        let q = TrackQueues::new();
        assert!(q.push("bogus", "x".to_string()).is_err());
    }
}
