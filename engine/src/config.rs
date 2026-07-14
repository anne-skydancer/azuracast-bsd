//! Config loading for the engine.
//!
//! The PHP side writes a TOML file (`engine.toml`) with a fixed shape; this
//! module treats that shape as a contract and does not attempt to be
//! flexible about it. See `engine/SPEC.md` and the Phase 2 task description
//! for the exact fields.

use serde::Deserialize;

#[derive(Debug, Deserialize, Clone)]
pub struct EngineConfig {
    pub station: StationConfig,
    pub control_api: ControlApiConfig,
    pub callbacks: CallbacksConfig,
    pub paths: PathsConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub struct StationConfig {
    pub id: i64,
    pub name: String,
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
