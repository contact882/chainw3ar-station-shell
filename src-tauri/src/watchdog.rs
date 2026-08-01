//! Watchdog: recover the webview, never the state — and NEVER mask failures.
//!
//! Auto-restart is fine; silent auto-restart is not. Every recovery is
//! logged with its reason and counter. Past the cap (3 in 60s) the watchdog
//! STOPS recovering and holds a persistent, shell-owned fault screen — a
//! station that stays down and says so beats one that flickers back to life
//! while the operator watches. Exit 70 happens only if even the fault screen
//! cannot be created (WebView2 environment itself is gone — the external
//! wrapper's problem, see DEPLOYMENT.md).

use crate::commands::ui::Heartbeat;
use crate::windows::{reload_via_com, WindowFactory};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};
use tokio::sync::mpsc::UnboundedReceiver;

#[derive(Debug, Clone, Copy)]
pub enum WatchdogEvent {
    /// COREWEBVIEW2_PROCESS_FAILED_KIND raw value.
    ProcessFailed(i32),
}

const KIND_BROWSER_PROCESS_EXITED: i32 = 0;
const HEARTBEAT_LIMIT_MS: u64 = 30_000;
pub const EXIT_STORM: i32 = 70;

fn kind_name(kind: i32) -> &'static str {
    match kind {
        0 => "browser process exited",
        1 => "render process exited",
        2 => "render process unresponsive",
        3 => "frame render process exited",
        4 => "utility process exited",
        5 => "sandbox helper exited",
        6 => "gpu process exited",
        _ => "unknown process failure",
    }
}

/// Recovery-storm gate: allows at most `limit` recoveries per `window`.
/// Extracted for unit testing — the policy is the point, not the plumbing.
pub struct StormGate {
    window: Duration,
    limit: usize,
    events: Vec<Instant>,
}

impl StormGate {
    pub fn new(window: Duration, limit: usize) -> Self {
        Self { window, limit, events: Vec::new() }
    }

    /// Registers a recovery attempt at `now`; returns (attempt_number,
    /// allowed). `allowed == false` means the cap is exceeded: stop
    /// recovering and enter the fault state.
    pub fn admit(&mut self, now: Instant) -> (usize, bool) {
        self.events.push(now);
        self.events.retain(|t| now.duration_since(*t) < self.window);
        let n = self.events.len();
        (n, n <= self.limit)
    }
}

pub fn spawn(
    app: AppHandle,
    mut rx: UnboundedReceiver<WatchdogEvent>,
    heartbeat: Arc<Heartbeat>,
) {
    tauri::async_runtime::spawn(async move {
        let mut gate = StormGate::new(Duration::from_secs(60), 3);
        let mut check = tokio::time::interval(Duration::from_secs(10));
        check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            // (recreate_window, reason)
            let action: Option<(bool, String)> = tokio::select! {
                event = rx.recv() => match event {
                    Some(WatchdogEvent::ProcessFailed(kind)) => {
                        Some((kind == KIND_BROWSER_PROCESS_EXITED,
                              format!("{} (kind {kind})", kind_name(kind))))
                    }
                    None => break,
                },
                _ = check.tick() => {
                    let age = heartbeat.age_ms();
                    if age > HEARTBEAT_LIMIT_MS {
                        heartbeat.touch(); // no immediate re-trigger while recovering
                        Some((false, format!("page heartbeat silent for {age}ms")))
                    } else {
                        None
                    }
                }
            };
            let Some((recreate_window, reason)) = action else { continue };

            let (attempt, allowed) = gate.admit(Instant::now());
            if !allowed {
                tracing::error!(
                    "recovery cap exceeded ({attempt} in 60s; last reason: {reason}) — \
                     entering persistent fault state, no further automatic recovery"
                );
                enter_fault_state(&app);
                break;
            }

            tracing::error!(
                "webview recovery {attempt}/3: {reason} → {}",
                if recreate_window { "recreating window" } else { "reloading document" }
            );
            if recreate_window {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.destroy();
                }
                let factory = app.state::<WindowFactory>();
                match factory.create_main(&app) {
                    Ok(_) => tracing::info!("main window recreated"),
                    Err(e) => tracing::error!("window recreation failed: {e}"),
                }
            } else if let Some(window) = app.get_webview_window("main") {
                reload_via_com(&window);
            }
        }
    });
}

/// Terminal: replace the UI with the static LINE PAUSED page and hold it.
/// The process stays alive so the fault stays visible; a human (or remote
/// service action) restarts the station deliberately.
fn enter_fault_state(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.destroy();
    }
    let factory = app.state::<WindowFactory>();
    match factory.create_fault(app) {
        Ok(_) => {
            tracing::error!(
                "persistent fault screen up — restart the station process to resume \
                 (state is safe on disk; recovery is lossless)"
            );
        }
        Err(e) => {
            tracing::error!(
                "fault screen creation failed ({e}) — WebView2 environment unusable; \
                 exiting {EXIT_STORM} for the restart wrapper"
            );
            app.exit(EXIT_STORM);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storm_gate_allows_up_to_limit_then_refuses() {
        let mut gate = StormGate::new(Duration::from_secs(60), 3);
        let t0 = Instant::now();
        assert_eq!(gate.admit(t0), (1, true));
        assert_eq!(gate.admit(t0 + Duration::from_secs(5)), (2, true));
        assert_eq!(gate.admit(t0 + Duration::from_secs(10)), (3, true));
        let (n, allowed) = gate.admit(t0 + Duration::from_secs(15));
        assert_eq!((n, allowed), (4, false), "4th recovery inside 60s must refuse");
    }

    #[test]
    fn storm_gate_forgets_old_events_outside_the_window() {
        let mut gate = StormGate::new(Duration::from_secs(60), 3);
        let t0 = Instant::now();
        gate.admit(t0);
        gate.admit(t0 + Duration::from_secs(1));
        gate.admit(t0 + Duration::from_secs(2));
        // 61s later the burst has aged out — recovery is allowed again.
        let (n, allowed) = gate.admit(t0 + Duration::from_secs(63));
        assert!(allowed, "old failures must not poison future recovery");
        assert_eq!(n, 1);
    }
}
