//! Media resolution: turns the bare path/URI produced by `annotate.rs` into
//! a local filesystem path the decoder can open, via the `cp` callback
//! (`engine/SPEC.md` D.6).
//!
//! **Scope simplification (deliberate):** SPEC.md's `azuracast.media_protocol`
//! (C.10) only calls `cp` when `settings.azuracast.media_path() == "api"`;
//! when media storage is local, it builds the path directly
//! (`"{media_storage_dir}/{arg}"`) with no HTTP round-trip at all. This
//! engine does not have that local-vs-api branch wired from PHP's station
//! config yet, so — as explicitly authorized by the task — it calls `cp`
//! *unconditionally* for every track, local or not. This is strictly
//! correct (PHP's `cp` handler works for local filesystems too, it just
//! costs an extra HTTP round trip per track), just not the fast path.
//! Wiring the local-path optimization back in is a reasonable follow-up
//! once `station.media_storage_location`/`isLocal()` is threaded into
//! `engine.toml`.

use std::path::PathBuf;
use std::time::Duration;

use crate::callbacks::CallbackClient;

/// A resolved local file, plus whether it should be deleted after use.
#[derive(Debug, Clone)]
pub struct ResolvedMedia {
    pub local_path: PathBuf,
    /// Mirrors `cp`'s `isTemp` flag (SPEC.md D.6): `true` when the path is a
    /// throwaway materialization (e.g. downloaded from non-local storage)
    /// that must be cleaned up once playback finishes, rather than a
    /// permanent on-disk file.
    pub is_temp: bool,
}

impl ResolvedMedia {
    /// Deletes the resolved file from disk if (and only if) it was flagged
    /// `isTemp` by the `cp` response. Safe to call even if the file was
    /// already removed (errors are logged, not propagated — a missing temp
    /// file at cleanup time isn't a reason to fail playback that already
    /// happened).
    pub fn cleanup(&self) {
        if self.is_temp {
            if let Err(e) = std::fs::remove_file(&self.local_path) {
                tracing::warn!(
                    "failed to remove temp media file {}: {e}",
                    self.local_path.display()
                );
            }
        }
    }
}

/// Default timeout for `cp` calls. SPEC.md D.6 notes the real Liquidsoap
/// caller computes a dynamic timeout (ms remaining until the request's
/// `maxtime`); this engine has no equivalent "request maxtime" concept yet,
/// so it uses a fixed, generous timeout instead (documented simplification).
pub const CP_TIMEOUT: Duration = Duration::from_secs(30);

/// Resolves `uri` (the bare path/URI extracted from a parsed annotation
/// string) to a local file via the `cp` callback.
pub async fn resolve_media(client: &CallbackClient, uri: &str) -> Result<ResolvedMedia, String> {
    let resp = client.call_cp(uri, CP_TIMEOUT).await?;
    Ok(ResolvedMedia {
        local_path: PathBuf::from(resp.uri),
        is_temp: resp.is_temp,
    })
}
