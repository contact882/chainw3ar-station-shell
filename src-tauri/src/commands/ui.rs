//! Commands the injected adapter calls — the async half of the bridge.
//! These are the ONLY commands reachable from the kiosk document in normal
//! operation (plus heartbeat/log plumbing).

use crate::boot::BootCache;
use crate::contract::{BatchInfo, PushPayload};
use crate::core::actor::CoreHandle;
use crate::core::messages::{CoreMsg, EndShiftReply};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tauri::State;

#[tauri::command]
pub async fn list_batches(core: State<'_, CoreHandle>) -> Result<Vec<BatchInfo>, String> {
    core.list_batches().await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn select_batch(id: String, core: State<'_, CoreHandle>) -> Result<PushPayload, String> {
    core.select_batch(id).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn end_shift(core: State<'_, CoreHandle>) -> Result<EndShiftReply, String> {
    core.end_shift().await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn bridge_attached(
    adapter_seq: u64,
    core: State<'_, CoreHandle>,
) -> Result<(), String> {
    core.send(CoreMsg::BridgeAttached { adapter_seq }).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub fn log_console(level: String, message: String) {
    crate::logging::log_ui_console(&level, &message);
}

/// Millisecond timestamp of the page's last heartbeat, for the watchdog.
pub struct Heartbeat(pub AtomicU64);

impl Heartbeat {
    pub fn now_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
    pub fn new() -> Arc<Self> {
        Arc::new(Self(AtomicU64::new(Self::now_ms())))
    }
    pub fn touch(&self) {
        self.0.store(Self::now_ms(), Ordering::Relaxed);
    }
    pub fn age_ms(&self) -> u64 {
        Self::now_ms().saturating_sub(self.0.load(Ordering::Relaxed))
    }
}

#[tauri::command]
pub fn heartbeat(hb: State<'_, Arc<Heartbeat>>) {
    hb.touch();
}

/// Async twin of the station://boot pull — sim console diagnostics.
#[tauri::command]
pub fn boot_snapshot(cache: State<'_, Arc<BootCache>>) -> Result<serde_json::Value, String> {
    serde_json::from_str(&cache.load()).map_err(|e| e.to_string())
}

