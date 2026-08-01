//! Sim console commands — the founder's demo driver while keying is stubbed.
//! Reachable ONLY from the window labeled "sim" under --sim. The kiosk
//! document cannot invoke these.

use super::{ensure_sim, Modes};
use crate::core::actor::CoreHandle;
use crate::core::messages::{CoreMsg, SimStateView};
use crate::seams::keying::{ForcedOutcome, SimKeying};
use std::sync::Arc;
use tauri::{State, Webview};

#[tauri::command]
pub async fn sim_present_chip(
    forced: Option<ForcedOutcome>,
    webview: Webview,
    modes: State<'_, Modes>,
    core: State<'_, CoreHandle>,
) -> Result<(), String> {
    ensure_sim(&webview, &modes)?;
    core.send(CoreMsg::CardPresented { reader_id: "sim-console".into(), atr: Vec::new(), forced })
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn sim_set_reader(
    present: bool,
    webview: Webview,
    modes: State<'_, Modes>,
    core: State<'_, CoreHandle>,
) -> Result<(), String> {
    ensure_sim(&webview, &modes)?;
    core.send(CoreMsg::ReaderPresence { present }).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn sim_set_downstream(
    ready: bool,
    webview: Webview,
    modes: State<'_, Modes>,
    core: State<'_, CoreHandle>,
) -> Result<(), String> {
    ensure_sim(&webview, &modes)?;
    core.send(CoreMsg::SetDownstream { ready }).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn sim_blip(
    ms: u64,
    webview: Webview,
    modes: State<'_, Modes>,
    core: State<'_, CoreHandle>,
) -> Result<(), String> {
    ensure_sim(&webview, &modes)?;
    core.send(CoreMsg::Blip { ms: ms.clamp(100, 60_000) }).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn sim_swap_batch(
    id: Option<String>,
    webview: Webview,
    modes: State<'_, Modes>,
    core: State<'_, CoreHandle>,
) -> Result<(), String> {
    ensure_sim(&webview, &modes)?;
    core.swap_batch(id).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn sim_end_shift(
    webview: Webview,
    modes: State<'_, Modes>,
    core: State<'_, CoreHandle>,
) -> Result<(), String> {
    ensure_sim(&webview, &modes)?;
    core.send(CoreMsg::BeginEndShift).await.map_err(|e| e.to_string())
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyPatch {
    pub forced_next: Option<ForcedOutcome>,
    pub weight_keyed: Option<u32>,
    pub weight_recoverable: Option<u32>,
    pub weight_permanent: Option<u32>,
    pub latency_ms: Option<u64>,
    /// Some("") clears the sticky chip; Some(other) sets it.
    pub sticky_chip: Option<String>,
}

#[tauri::command]
pub fn sim_arm_policy(
    patch: PolicyPatch,
    webview: Webview,
    modes: State<'_, Modes>,
    keying: State<'_, Arc<SimKeying>>,
) -> Result<(), String> {
    ensure_sim(&webview, &modes)?;
    keying.arm(|p| {
        if let Some(f) = patch.forced_next {
            p.forced_next = Some(f);
        }
        if let Some(w) = patch.weight_keyed {
            p.weight_keyed = w;
        }
        if let Some(w) = patch.weight_recoverable {
            p.weight_recoverable = w;
        }
        if let Some(w) = patch.weight_permanent {
            p.weight_permanent = w;
        }
        if let Some(l) = patch.latency_ms {
            p.latency_ms = l.min(10_000);
        }
        if let Some(s) = patch.sticky_chip {
            p.sticky_chip = if s.is_empty() { None } else { Some(s) };
        }
    });
    Ok(())
}

#[tauri::command]
pub async fn sim_get_state(
    webview: Webview,
    modes: State<'_, Modes>,
    core: State<'_, CoreHandle>,
) -> Result<SimStateView, String> {
    ensure_sim(&webview, &modes)?;
    core.sim_state().await.map_err(|e| e.to_string())
}

/// Demo-data reset: wipes accumulated sim batch progress (the "525/500 at
/// every demo" source) + the persisted session, then reloads the UI so it
/// boots fresh. Sim window only — never an operator affordance.
#[tauri::command]
pub async fn sim_reset_demo(
    webview: Webview,
    modes: State<'_, Modes>,
    core: State<'_, CoreHandle>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    ensure_sim(&webview, &modes)?;
    core.reset_demo().await.map_err(|e| e.to_string())?;
    if let Some(window) = tauri::Manager::get_webview_window(&app, "main") {
        crate::windows::reload_via_com(&window);
    }
    Ok(())
}

#[tauri::command]
pub fn sim_quit(webview: Webview, modes: State<'_, Modes>, app: tauri::AppHandle) -> Result<(), String> {
    ensure_sim(&webview, &modes)?;
    app.exit(0);
    Ok(())
}
