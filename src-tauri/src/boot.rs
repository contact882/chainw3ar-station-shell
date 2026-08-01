//! The boot snapshot cache and the `station://` custom protocol.
//!
//! The init script pulls `GET http://station.localhost/boot` with a
//! SYNCHRONOUS XHR on every document creation, so a reload hours into a shift
//! boots from current truth (initialization scripts are fixed strings — a
//! baked snapshot would be stale by then). The handler must stay
//! microseconds-cheap and lock-free: it runs on the host UI thread while the
//! renderer's JS thread blocks. ArcSwap load + clone of pre-serialized bytes,
//! nothing else.

use crate::contract::BootPayload;
use arc_swap::ArcSwap;
use std::sync::Arc;

pub struct BootCache {
    json: ArcSwap<String>,
}

impl BootCache {
    pub fn new(initial: &BootPayload) -> Self {
        Self {
            json: ArcSwap::from_pointee(
                serde_json::to_string(initial).expect("boot payload serializes"),
            ),
        }
    }

    pub fn store(&self, payload: &BootPayload) {
        if let Ok(json) = serde_json::to_string(payload) {
            self.json.store(Arc::new(json));
        }
    }

    pub fn load(&self) -> Arc<String> {
        self.json.load_full()
    }
}

/// Response for the custom protocol. CORS: the page origin is
/// http://tauri.localhost, and a cross-origin sync GET is a "simple request"
/// — no preflight, but the response must carry Access-Control-Allow-Origin.
pub fn boot_response(cache: &BootCache) -> tauri::http::Response<Vec<u8>> {
    tauri::http::Response::builder()
        .status(200)
        .header("Content-Type", "application/json")
        .header("Access-Control-Allow-Origin", "*")
        .header("Cache-Control", "no-store")
        .body(cache.load().as_bytes().to_vec())
        .expect("static response builds")
}

pub fn not_found() -> tauri::http::Response<Vec<u8>> {
    tauri::http::Response::builder()
        .status(404)
        .header("Access-Control-Allow-Origin", "*")
        .body(Vec::new())
        .expect("static response builds")
}

/// The persistent fault page the watchdog holds after the recovery cap.
/// Shell-owned, zero scripts, no reload loop, outcome vocabulary only —
/// a station that stays down and says so, instead of flickering.
pub fn fault_page() -> tauri::http::Response<Vec<u8>> {
    const HTML: &str = r#"<!doctype html><html lang="en" style="background:#131619">
<head><meta charset="utf-8"><title>Station</title><style>
  body{margin:0;height:100vh;display:flex;flex-direction:column;align-items:center;
       justify-content:center;gap:28px;background:#131619;color:#8b949e;
       font:600 34px/1.2 "Segoe UI",sans-serif;letter-spacing:.12em}
  .glyph{display:flex;gap:14px}.glyph i{width:22px;height:72px;background:#8b949e;border-radius:6px}
</style></head>
<body><div class="glyph"><i></i><i></i></div><div>LINE PAUSED</div></body></html>"#;
    tauri::http::Response::builder()
        .status(200)
        .header("Content-Type", "text/html; charset=utf-8")
        .body(HTML.as_bytes().to_vec())
        .expect("static response builds")
}
