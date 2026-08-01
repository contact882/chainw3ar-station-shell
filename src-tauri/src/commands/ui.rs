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

/// Page-heartbeat tracker for the watchdog. MONOTONIC by construction:
/// wall-clock (SystemTime) is settable — a forward NTP/manual jump >30s
/// would have faked a silent page and triggered a spurious reload. Instant
/// cannot jump.
pub struct Heartbeat {
    start: std::time::Instant,
    last_ms: AtomicU64, // ms since `start` at the last beat
}

impl Heartbeat {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { start: std::time::Instant::now(), last_ms: AtomicU64::new(0) })
    }
    pub fn touch(&self) {
        self.last_ms
            .store(self.start.elapsed().as_millis() as u64, Ordering::Relaxed);
    }
    pub fn age_ms(&self) -> u64 {
        (self.start.elapsed().as_millis() as u64)
            .saturating_sub(self.last_ms.load(Ordering::Relaxed))
    }
}

#[cfg(test)]
mod heartbeat_tests {
    use super::Heartbeat;

    #[test]
    fn age_is_monotonic_elapsed_since_touch_not_wall_clock() {
        let hb = Heartbeat::new();
        hb.touch();
        assert!(hb.age_ms() < 250, "fresh touch reads near-zero age");
        std::thread::sleep(std::time::Duration::from_millis(60));
        let age = hb.age_ms();
        assert!((40..5_000).contains(&age), "age tracks monotonic elapsed, got {age}ms");
        hb.touch();
        assert!(hb.age_ms() < 250, "touch resets age");
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

