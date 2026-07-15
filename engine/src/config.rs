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
    /// New post-cutover (replaces the old Liquidsoap-only `nrj`/`master_me`/
    /// `stereo_tool` post-processing chain, which had no Rust equivalent).
    /// Absent entirely from a config file still parses fine (falls back to
    /// `AudioProcessingConfig::default()`, i.e. `method = None` -- no
    /// processing applied). See `audio_processing.rs`.
    #[serde(default)]
    pub audio_processing: AudioProcessingConfig,
    /// New in Phase 5 (SPEC.md B.7/B.14). Present only if the station has a
    /// local Icecast frontend at all; entirely absent means no local
    /// frontend and `mounts` (below) should be treated as meaningless even
    /// if somehow non-empty (see `output.rs::build_targets`).
    #[serde(default)]
    pub icecast_output: Option<IcecastOutputConfig>,
    /// New in Phase 5 (SPEC.md B.7). Zero or more local Icecast mount
    /// points, only meaningful alongside `icecast_output`.
    #[serde(default)]
    pub mounts: Vec<MountConfig>,
    /// New in Phase 5 (SPEC.md B.9). Zero or more third-party relay
    /// targets, independent of `icecast_output`/`mounts` -- a station can
    /// relay to remote servers with or without also running its own local
    /// Icecast frontend.
    #[serde(default)]
    pub remotes: Vec<RemoteConfig>,
    /// New post-cutover (SPEC.md B.8, deferred from Phase 5, now
    /// implemented). Present only when the station has `enable_hls` and at
    /// least one `StationHlsStream` configured -- see `hls.rs`.
    #[serde(default)]
    pub hls: Option<HlsConfig>,
    /// New post-cutover (SPEC.md B.8). One entry per configured HLS
    /// rendition (bitrate ladder), only meaningful alongside `hls`.
    #[serde(default)]
    pub hls_streams: Vec<HlsStreamConfig>,
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

/// Audio post-processing method selection (`StationBackendConfiguration::
/// audio_processing_method`, PHP-side `AudioProcessingMethods` enum). Every
/// field defaults sensibly when the section (or an individual field within
/// it) is absent, matching `CrossfadeConfig`/`HarborConfig`'s established
/// pattern -- an absent section means no processing at all, never a startup
/// error.
#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct AudioProcessingConfig {
    /// One of `"none"`, `"nrj"`, `"stereo_tool"`. Unrecognized values are
    /// treated the same as `"none"` (logged, not a config error) -- see
    /// `audio_processing::Processor::from_config`.
    pub method: String,
    /// Whether post-processing applies to live-DJ chunks too, or only to
    /// AutoDJ-sourced audio (`StationBackendConfiguration::
    /// post_processing_include_live`). Meaningless when `method == "none"`.
    pub include_live: bool,
    /// Absolute path to the operator-installed `stereo_tool` CLI binary.
    /// Only present (and only meaningful) when `method == "stereo_tool"`.
    #[serde(default)]
    pub stereo_tool_binary: Option<String>,
    /// Absolute path to the station's uploaded Stereo Tool preset/config
    /// file, passed to the binary's `-s` flag.
    #[serde(default)]
    pub stereo_tool_preset_path: Option<String>,
    /// Omitted entirely (not empty-string) when the station has no license
    /// key configured -- Stereo Tool runs in a functionality-limited demo
    /// mode in that case, same as the historical Liquidsoap integration.
    #[serde(default)]
    pub stereo_tool_license_key: Option<String>,
}

impl Default for AudioProcessingConfig {
    fn default() -> Self {
        Self {
            method: "none".to_string(),
            include_live: false,
            stereo_tool_binary: None,
            stereo_tool_preset_path: None,
            stereo_tool_license_key: None,
        }
    }
}

/// Local Icecast frontend connection info (SPEC.md B.7/B.14's
/// `output.icecast(host=..., port=..., password=...)` target). New in
/// Phase 5. Present in the TOML only when the station actually runs a
/// local Icecast frontend at all; see `output.rs::build_targets` for what
/// happens if `[[mounts]]` entries exist without this section (defensive
/// warn-and-skip, since PHP's contract says this shouldn't happen).
#[derive(Debug, Deserialize, Clone)]
pub struct IcecastOutputConfig {
    pub host: String,
    pub port: u16,
    pub source_password: String,
}

/// One local Icecast mount point (SPEC.md B.7 `writeLocalBroadcastConfiguration`
/// / B.14 `getOutputString`). New in Phase 5. `format` is one of `"mp3"`,
/// `"aac"`, `"ogg"`, `"opus"`, `"flac"` (see `output::OutputFormat::parse`);
/// an unrecognized value is logged and that single mount is skipped rather
/// than failing config load entirely.
#[derive(Debug, Deserialize, Clone)]
pub struct MountConfig {
    pub path: String,
    pub format: String,
    pub bitrate: u32,
    pub is_public: bool,
}

/// One third-party relay target (SPEC.md B.9 `writeRemoteBroadcastConfiguration`).
/// New in Phase 5. `protocol` is a fixed contract field from PHP, but this
/// engine only implements `"icecast"` -- any other value (e.g. legacy
/// Shoutcast/RSAS) is logged as a warning and that remote is skipped, not
/// treated as a config error (see `output.rs::build_targets`).
#[derive(Debug, Deserialize, Clone)]
pub struct RemoteConfig {
    pub host: String,
    pub port: u16,
    pub mount: String,
    /// Omitted entirely (rather than empty-string) when the station has no
    /// explicit source username configured. `output.rs` falls back to the
    /// conventional Icecast/Liquidsoap default username `"source"` in that
    /// case -- see its doc comment.
    #[serde(default)]
    pub username: Option<String>,
    pub password: String,
    pub format: String,
    pub bitrate: u32,
    pub is_public: bool,
    pub protocol: String,
}

/// File-based HLS segmenting output (SPEC.md B.8's `output.file.hls(...)`
/// call). New post-cutover, deferred from Phase 5. Unlike
/// `[icecast_output]`, this isn't a network target -- `base_dir` is a local
/// filesystem path (`station->getRadioHlsDir()`) that nginx serves directly
/// (`Nginx\ConfigWriter::writeHlsSection`, unchanged by this engine).
#[derive(Debug, Deserialize, Clone)]
pub struct HlsConfig {
    pub base_dir: String,
    /// SPEC.md's `hls_segment_length` (seconds), default 4.
    pub segment_secs: f64,
    /// SPEC.md's `hls_segments_in_playlist`, default 5.
    pub segments_in_playlist: u32,
    /// SPEC.md's `hls_segments_overhead`, default 2 -- how many extra
    /// already-rolled-off segments ffmpeg keeps on disk before deleting
    /// them (`-hls_delete_threshold`), matching the old system's grace
    /// window for in-flight client requests.
    pub segments_overhead: u32,
}

/// One HLS rendition (SPEC.md B.8's per-stream tuple in `hls_streams`). New
/// post-cutover. Always encoded as AAC-LC regardless of the station's
/// nominally-configured `HlsStreamProfiles` value -- see `hls.rs`'s module
/// doc for why (same `libfdk_aac`-avoidance constraint `output.rs` already
/// documents for its own AAC encoding).
#[derive(Debug, Deserialize, Clone)]
pub struct HlsStreamConfig {
    pub name: String,
    pub bitrate: u32,
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
