//! Config loading for the engine.
//!
//! The PHP side writes a TOML file (`engine.toml`) with a fixed shape; this
//! module treats that shape as a contract and does not attempt to be
//! flexible about it. See `engine/SPEC.md` and the Phase 2 task description
//! for the exact fields.

use serde::Deserialize;

use crate::crossfade::{CrossfadeMode, CrossfadeThresholds, DEFAULT_HIGH, DEFAULT_MARGIN, DEFAULT_MEDIUM};

#[derive(Debug, Deserialize, Clone)]
pub struct EngineConfig {
    pub station: StationConfig,
    pub control_api: ControlApiConfig,
    pub callbacks: CallbacksConfig,
    pub paths: PathsConfig,
    /// New in Phase 3. Absent entirely from a config file still parses
    /// fine (falls back to `CrossfadeConfig::default()`); PHP-side
    /// `StreamEngine::getCurrentConfiguration()` isn't wired to populate
    /// this yet -- see the Phase 3 report for follow-up.
    #[serde(default)]
    pub crossfade: CrossfadeConfig,
    /// New in Phase 4 (SPEC.md B.4 `writeHarborConfiguration`). Absent
    /// entirely from a config file still parses fine (falls back to
    /// `HarborConfig::default()`, i.e. `enabled = false` -- matches B.4's
    /// own "no-op if `!station->enable_streamers`" behavior).
    #[serde(default)]
    pub harbor: HarborConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub struct StationConfig {
    pub id: i64,
    pub name: String,
    /// New in Phase 3 (SPEC.md A.1 `enable_replaygain_metadata`). Defaults
    /// to `false` when absent -- PHP doesn't populate this field yet, see
    /// the Phase 3 report for the `StreamEngine::getCurrentConfiguration()`
    /// follow-up.
    #[serde(default)]
    pub replaygain_enabled: bool,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ControlApiConfig {
    pub bind_address: String,
    pub port: u16,
    pub api_key: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct CallbacksConfig {
    pub base_url: String,
    pub api_key: String,
    pub station_id: i64,
}

#[derive(Debug, Deserialize, Clone)]
pub struct PathsConfig {
    pub log_file: String,
    /// New in Phase 3 (SPEC.md C.8 `settings.azuracast.fallback_path`).
    /// Absent/unset -> silence is the acceptable degraded fallback
    /// behavior instead of a looped error file (see `autodj.rs`).
    #[serde(default)]
    pub fallback_file_path: Option<String>,
    /// New in Phase 3, not part of SPEC.md's original settings surface:
    /// the live pipeline's local-file-sink output path (raw interleaved
    /// f32 PCM @ 44100Hz stereo -- see `pipeline.rs` for why this isn't a
    /// WAV file). Absent/unset -> the pipeline runs without writing any
    /// audio out at all (useful for exercising the control API/AutoDJ
    /// logic without caring about the resulting audio).
    #[serde(default)]
    pub pipeline_output_path: Option<String>,
}

/// Per-station crossfade tuning (SPEC.md A.1's `crossfade`,
/// `crossfade_type`, `crossfade_smart_high/medium/margin` fields). New in
/// Phase 3; PHP-side config generation isn't wired to populate a
/// `[crossfade]` TOML section yet, so every field defaults to SPEC.md's own
/// stated defaults when the section (or individual fields within it) is
/// absent -- see the Phase 3 report for the follow-up to wire this from
/// `StationBackendConfiguration` for real.
#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct CrossfadeConfig {
    /// `"smart"`, `"normal"`, or `"disabled"`/`"none"`. Unrecognized values
    /// fall back to `"normal"` (SPEC.md A.2's stated enum default).
    pub mode: String,
    /// SPEC.md A.1 `crossfade` (`default_fade`), seconds.
    pub fade_seconds: f64,
    /// SPEC.md A.1 `crossfade_smart_high`.
    pub high: f64,
    /// SPEC.md A.1 `crossfade_smart_medium`.
    pub medium: f64,
    /// SPEC.md A.1 `crossfade_smart_margin`.
    pub margin: f64,
}

impl Default for CrossfadeConfig {
    fn default() -> Self {
        Self {
            mode: "normal".to_string(),
            fade_seconds: crate::crossfade::DEFAULT_FADE_SECONDS,
            high: DEFAULT_HIGH,
            medium: DEFAULT_MEDIUM,
            margin: DEFAULT_MARGIN,
        }
    }
}

impl CrossfadeConfig {
    pub fn mode(&self) -> CrossfadeMode {
        match self.mode.as_str() {
            "smart" => CrossfadeMode::Smart,
            "disabled" | "none" => CrossfadeMode::Disabled,
            _ => CrossfadeMode::Normal,
        }
    }

    pub fn thresholds(&self) -> CrossfadeThresholds {
        CrossfadeThresholds {
            high: self.high,
            medium: self.medium,
            margin: self.margin,
        }
    }
}

/// Live-DJ harbor input (SPEC.md B.4's `input.harbor(...)` call). New in
/// Phase 4; PHP's `StreamEngine::getCurrentConfiguration()` writes this
/// section as a fixed contract -- see the Phase 4 task description. Every
/// field defaults sensibly when the section (or an individual field within
/// it) is absent, matching `CrossfadeConfig`'s established pattern.
#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct HarborConfig {
    /// Gates the entire harbor listener: mirrors B.4's
    /// `!station->enable_streamers` no-op check. `false` (the default) means
    /// the engine does not bind/listen at all.
    pub enabled: bool,
    pub bind_address: String,
    pub port: u16,
    /// The single source-client mount point this harbor instance accepts
    /// (SPEC.md's `input.harbor(mount, ...)`). Connections requesting a
    /// different mount are rejected.
    pub mount_point: String,
    pub charset: String,
    /// SPEC.md B.4: `buffer=N.` -- only present (station-configured) when
    /// the station sets a non-zero DJ buffer. `None` when absent -- this
    /// engine does not yet implement input buffering depth as a tunable
    /// (its live decode path is unbuffered/streaming by construction), so
    /// this field is threaded through/logged but not otherwise acted on.
    pub buffer_secs: Option<f64>,
    /// SPEC.md B.4: `max=max(N+5,10).` -- only present alongside
    /// `buffer_secs`. Same "logged, not acted on" status as `buffer_secs`.
    pub max_buffer_secs: Option<f64>,
}

impl Default for HarborConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bind_address: "0.0.0.0".to_string(),
            port: 8005,
            mount_point: "/".to_string(),
            charset: "UTF-8".to_string(),
            buffer_secs: None,
            max_buffer_secs: None,
        }
    }
}

/// Reads and parses the config file at `path`. Fails fast (returns `Err`)
/// with a human-readable message if the file is missing, unreadable, or not
/// valid TOML matching the expected shape.
pub fn load_config(path: &str) -> Result<EngineConfig, String> {
    let contents = std::fs::read_to_string(path)
        .map_err(|e| format!("failed to read config file '{path}': {e}"))?;
    parse_config(&contents)
}

/// Parses an already-loaded TOML document (used by both `load_config` and
/// `--check-config -`, which reads the document from stdin instead of a
/// file).
pub fn parse_config(contents: &str) -> Result<EngineConfig, String> {
    toml::from_str::<EngineConfig>(contents).map_err(|e| format!("failed to parse config: {e}"))
}
