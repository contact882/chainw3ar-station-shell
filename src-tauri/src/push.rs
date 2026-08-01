//! Push channel: Rust → page. One eval per payload keeps snapshot+events
//! atomic on the JS side; the JSON rides inside a JSON string literal
//! (`JSON.parse("…")`) so nothing in the payload — batch names included — can
//! escape into script context, and U+2028/29 quirks are moot.
//!
//! Pushes target ONLY the window labeled "main": exactly one UI window per
//! bridge, enforced here as well as at window creation.

use crate::contract::PushPayload;
use tauri::{AppHandle, Manager, Runtime};

pub trait Pusher: Send + Sync {
    fn push(&self, payload: &PushPayload);
}

pub struct EvalPusher<R: Runtime> {
    app: AppHandle<R>,
}

impl<R: Runtime> EvalPusher<R> {
    pub fn new(app: AppHandle<R>) -> Self {
        Self { app }
    }
}

impl<R: Runtime> Pusher for EvalPusher<R> {
    fn push(&self, payload: &PushPayload) {
        let Some(window) = self.app.get_webview_window("main") else {
            // Window mid-recreation: safe to drop — the fresh document's boot
            // pull carries this state anyway (that is the whole design).
            tracing::warn!("push dropped: main window absent (seq {})", payload.seq);
            return;
        };
        let json = match serde_json::to_string(payload) {
            Ok(j) => j,
            Err(e) => {
                tracing::error!("push serialize failed: {e}");
                return;
            }
        };
        let literal = serde_json::to_string(&json).expect("string re-encode");
        if let Err(e) = window.eval(format!("window.__STATION_PUSH__(JSON.parse({literal}))")) {
            tracing::warn!("push eval failed (seq {}): {e}", payload.seq);
        }
    }
}

/// Test double: records every payload.
#[cfg(test)]
pub struct VecPusher(pub std::sync::Mutex<Vec<PushPayload>>);

#[cfg(test)]
impl VecPusher {
    pub fn new() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self(std::sync::Mutex::new(Vec::new())))
    }
    pub fn take(&self) -> Vec<PushPayload> {
        std::mem::take(&mut self.0.lock().unwrap())
    }
}

#[cfg(test)]
impl Pusher for VecPusher {
    fn push(&self, payload: &PushPayload) {
        self.0.lock().unwrap().push(payload.clone());
    }
}
