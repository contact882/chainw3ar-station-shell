//! Station deployment configuration (station.toml). Everything here is
//! station-level and static for the process lifetime — the UI reads `config`
//! once at boot and treats it as immutable.

use crate::contract::FailBinSide;
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// What happens when a tap arrives with the batch already at/past total.
/// Chips are physically consumed by keying — this is a licensing/inventory
/// policy, not a display choice (the frozen UI always shows true numbers).
/// Real enforcement ultimately belongs to the governed layer, which owns
/// batch authorization; this is the shell-side stopgap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OverrunPolicy {
    /// Keep keying and counting; every overrun success writes a distinct
    /// OVERRUN record (founder decision 2026-08-01).
    Allow,
    /// Presentations past total produce no keying call and no verdict.
    Block,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct StationConfig {
    pub fail_bin_side: FailBinSide,
    /// Substring match selecting this station's PC/SC reader.
    pub reader_match: String,
    pub overrun_policy: OverrunPolicy,
    /// Optional operator-string overrides (localization) → __STATION_STRINGS__.
    pub strings: Option<serde_json::Value>,
    pub sim: SimConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SimConfig {
    pub weight_keyed: u32,
    pub weight_recoverable: u32,
    pub weight_permanent: u32,
    pub latency_ms: u64,
}

impl Default for StationConfig {
    fn default() -> Self {
        Self {
            fail_bin_side: FailBinSide::Right,
            reader_match: "uTrust 3700".into(),
            overrun_policy: OverrunPolicy::Allow,
            strings: None,
            sim: SimConfig::default(),
        }
    }
}

impl Default for SimConfig {
    fn default() -> Self {
        Self { weight_keyed: 85, weight_recoverable: 12, weight_permanent: 3, latency_ms: 250 }
    }
}

impl StationConfig {
    /// Explicit path > exe-adjacent station.toml > built-in defaults.
    pub fn load(explicit: Option<&Path>) -> Self {
        let candidate = explicit
            .map(Path::to_path_buf)
            .or_else(|| {
                std::env::current_exe()
                    .ok()
                    .and_then(|exe| exe.parent().map(|d| d.join("station.toml")))
                    .filter(|p| p.exists())
            })
            // Dev convenience: cargo run's cwd is src-tauri.
            .or_else(|| Some(PathBuf::from("station.toml")).filter(|p| p.exists()));
        let Some(path) = candidate else {
            tracing::info!("no station.toml — using built-in defaults");
            return Self::default();
        };
        match std::fs::read_to_string(&path).map_err(anyhow::Error::from).and_then(|raw| {
            toml::from_str::<StationConfig>(&raw).map_err(anyhow::Error::from)
        }) {
            Ok(config) => {
                tracing::info!("config loaded from {}", path.display());
                config
            }
            Err(e) => {
                tracing::error!("config {} unreadable ({e}); using defaults", path.display());
                Self::default()
            }
        }
    }
}
