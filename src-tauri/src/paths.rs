//! Data locations. Matches Tauri's app-data convention for this identifier
//! (%APPDATA%\com.chainw3ar.station) without needing an AppHandle, so logging
//! and persistence can initialize before the Tauri builder runs.

use std::path::PathBuf;

pub const IDENTIFIER: &str = "com.chainw3ar.station";

pub fn data_dir() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(IDENTIFIER)
}

pub fn log_dir() -> PathBuf {
    data_dir().join("logs")
}
