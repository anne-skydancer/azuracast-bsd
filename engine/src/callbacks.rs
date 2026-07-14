//! HTTP client for the seven "reverse callback" endpoints the engine calls
//! back into the PHP application, per `engine/SPEC.md` section D.
//!
//! ## Contract extracted from SPEC.md D.0
//!
//! - Route: `POST /api/internal/{station_id}/liquidsoap/{action}` where
//!   `action` is one of `cp`, `auth`, `djon`, `djoff`, `feedback`,
//!   `nextsong`, `savecache`. (D.0 documents the route as `GET|POST`, but
//!   the actual Liquidsoap-side caller — `azuracast.api_call`, C.2 — always
//!   uses `http.post`, so this client always POSTs.)
//! - Full URL: `{callbacks.base_url}/api/internal/{callbacks.station_id}/liquidsoap/{action}`
//!   (`callbacks.base_url` plays the role of Liquidsoap's `settings.azuracast.api_url`
//!   minus the `/api/internal/{station_id}/liquidsoap` suffix, which this
//!   client appends itself).
//! - Auth: header `X-Liquidsoap-Api-Key: {callbacks.api_key}`.
//! - Headers: `Content-Type: application/json`, `User-Agent: Liquidsoap AzuraCast`.
//! - Body: JSON-stringified payload, except `nextsong` which sends a
//!   literal empty string body (no JSON at all — see D.1).
//! - Response: HTTP 200 with a JSON body on success. Any non-200 status is
//!   treated as a hard failure (mirrors `azuracast.api_call`, which folds
//!   both transport errors and non-200 responses into a single `null`
//!   result and never inspects the error body).

use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::Duration;

use crate::config::CallbacksConfig;

/// Default per-call timeout, mirroring `settings.azuracast.http_timeout` (C.1).
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
/// `auth` uses its own fixed 5s timeout (D.2), distinct from the general default.
const AUTH_TIMEOUT: Duration = Duration::from_secs(5);
/// `savecache` also uses a fixed 5s timeout (D.7).
const SAVECACHE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Deserialize, Serialize)]
pub struct NextSongResponse {
    pub uri: String,
}

#[derive(Debug, Deserialize, Serialize, Default)]
pub struct DjAuthResponse {
    pub allow: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

/// Metadata subset sent to `feedback`, per C.6. All fields are optional and
/// omitted from the JSON body when absent.
#[derive(Debug, Serialize, Default)]
pub struct FeedbackPayload {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub song_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub playlist_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sq_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CpResponse {
    pub uri: String,
    #[serde(rename = "isTemp")]
    pub is_temp: bool,
}

/// Client for the seven reverse callbacks documented in `engine/SPEC.md` section D.
#[derive(Debug, Clone)]
pub struct CallbackClient {
    http: reqwest::Client,
    base_url: String,
    station_id: i64,
    api_key: String,
}

impl CallbackClient {
    pub fn new(cfg: &CallbacksConfig) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: cfg.base_url.trim_end_matches('/').to_string(),
            station_id: cfg.station_id,
            api_key: cfg.api_key.clone(),
        }
    }

    fn endpoint(&self, action: &str) -> String {
        format!(
            "{}/api/internal/{}/liquidsoap/{}",
            self.base_url, self.station_id, action
        )
    }

    /// Shared POST primitive mirroring `azuracast.api_call` (C.2): always
    /// POSTs JSON with the fixed headers, treats non-200 as a failure, and
    /// returns the raw response body on success (callers parse it
    /// themselves, matching the Liquidsoap-side pattern of not
    /// pre-parsing).
    async fn post(&self, action: &str, body: String, timeout: Duration) -> Result<String, String> {
        let url = self.endpoint(action);
        let resp = self
            .http
            .post(&url)
            .header("Content-Type", "application/json")
            .header("User-Agent", "Liquidsoap AzuraCast")
            .header("X-Liquidsoap-Api-Key", &self.api_key)
            .timeout(timeout)
            .body(body)
            .send()
            .await
            .map_err(|e| format!("request to {url} failed: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            return Err(format!("{url} returned non-200 status: {status}"));
        }

        resp.text()
            .await
            .map_err(|e| format!("failed to read response body from {url}: {e}"))
    }

    /// D.1 `nextsong` — empty-string payload (no request-side parameters at
    /// all). Wired into an active polling loop in this phase (see
    /// `main.rs`); the other six callbacks below are spec-correct but not
    /// yet triggered by a real event source.
    pub async fn call_nextsong(&self) -> Result<NextSongResponse, String> {
        let body = self.post("nextsong", String::new(), DEFAULT_TIMEOUT).await?;
        serde_json::from_str(&body)
            .map_err(|e| format!("failed to parse nextsong response: {e} (body: {body})"))
    }

    /// D.2 `auth` — `auth_info` is whatever fields the source-client's
    /// ICY/source-protocol handshake supplies (practically `user`/
    /// `password` at minimum); left as a generic JSON value since harbor
    /// doesn't fix the field set.
    pub async fn call_auth(&self, auth_info: serde_json::Value) -> Result<DjAuthResponse, String> {
        let body = serde_json::to_string(&auth_info).map_err(|e| e.to_string())?;
        let resp_body = self.post("auth", body, AUTH_TIMEOUT).await?;
        serde_json::from_str(&resp_body)
            .map_err(|e| format!("failed to parse auth response: {e} (body: {resp_body})"))
    }

    /// D.3 `djon` — payload `{"user": ...}`. Response is ignored by the
    /// real Liquidsoap caller, but surfaced here for logging.
    pub async fn call_djon(&self, user: &str) -> Result<bool, String> {
        let body = serde_json::to_string(&json!({ "user": user })).map_err(|e| e.to_string())?;
        let resp_body = self.post("djon", body, DEFAULT_TIMEOUT).await?;
        serde_json::from_str(&resp_body)
            .map_err(|e| format!("failed to parse djon response: {e} (body: {resp_body})"))
    }

    /// D.4 `djoff` — payload `{"user": ...}` (the PHP side does not
    /// actually read `user`, but the `.liq` caller sends it regardless, so
    /// this client does too for exactness).
    pub async fn call_djoff(&self, user: &str) -> Result<bool, String> {
        let body = serde_json::to_string(&json!({ "user": user })).map_err(|e| e.to_string())?;
        let resp_body = self.post("djoff", body, DEFAULT_TIMEOUT).await?;
        serde_json::from_str(&resp_body)
            .map_err(|e| format!("failed to parse djoff response: {e} (body: {resp_body})"))
    }

    /// D.5 `feedback` — fire-and-forget from the `.liq` caller's
    /// perspective (it discards the result); this client still surfaces
    /// errors so the engine can log them.
    pub async fn call_feedback(&self, payload: FeedbackPayload) -> Result<bool, String> {
        let body = serde_json::to_string(&payload).map_err(|e| e.to_string())?;
        let resp_body = self.post("feedback", body, DEFAULT_TIMEOUT).await?;
        serde_json::from_str(&resp_body)
            .map_err(|e| format!("failed to parse feedback response: {e} (body: {resp_body})"))
    }

    /// D.6 `cp` — payload `{"uri": ...}`. The real caller computes a
    /// dynamic timeout (ms remaining until the request's `maxtime`); this
    /// client takes that as a parameter rather than hardcoding it.
    pub async fn call_cp(&self, uri: &str, timeout: Duration) -> Result<CpResponse, String> {
        let body = serde_json::to_string(&json!({ "uri": uri })).map_err(|e| e.to_string())?;
        let resp_body = self.post("cp", body, timeout).await?;
        serde_json::from_str(&resp_body)
            .map_err(|e| format!("failed to parse cp response: {e} (body: {resp_body})"))
    }

    /// D.7 `savecache` — payload `{"cache_key": ..., "data": {...}}`,
    /// fixed 5s timeout, fire-and-forget from the `.liq` caller.
    pub async fn call_savecache(
        &self,
        cache_key: &str,
        data: serde_json::Value,
    ) -> Result<bool, String> {
        let body = serde_json::to_string(&json!({ "cache_key": cache_key, "data": data }))
            .map_err(|e| e.to_string())?;
        let resp_body = self.post("savecache", body, SAVECACHE_TIMEOUT).await?;
        serde_json::from_str(&resp_body)
            .map_err(|e| format!("failed to parse savecache response: {e} (body: {resp_body})"))
    }
}
