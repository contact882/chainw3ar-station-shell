//! Conformance harness commands — the mechanical acceptance gate's Rust half.
//! Reachable ONLY from the "main" window under --conformance (the harness
//! page loads there so the production injection path is what gets tested).

use super::{ensure_conformance, ensure_sim, Modes};
use crate::contract::TapVerdict;
use crate::core::actor::CoreHandle;
use crate::core::messages::CoreMsg;
use crate::seams::keying::force_for_verdict;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{State, Webview};

#[tauri::command]
pub async fn conformance_reset(
    webview: Webview,
    modes: State<'_, Modes>,
    core: State<'_, CoreHandle>,
) -> Result<(), String> {
    ensure_conformance(&webview, &modes)?;
    core.conformance_reset().await.map_err(|e| e.to_string())
}

/// Forces exactly the requested verdict THROUGH THE REAL PIPELINE: the armed
/// outcome maps success→Keyed, retry→Recoverable, dead→Permanent, and the sim
/// mints a fresh chip_ref per presentation, so a single Recoverable can never
/// trip the 3-strikes escalation into an unexpected dead.
#[tauri::command]
pub async fn conformance_fire_tap(
    verdict: TapVerdict,
    webview: Webview,
    modes: State<'_, Modes>,
    core: State<'_, CoreHandle>,
) -> Result<(), String> {
    ensure_conformance(&webview, &modes)?;
    core.send(CoreMsg::CardPresented {
        reader_id: "conformance".into(),
        atr: Vec::new(),
        forced: Some(force_for_verdict(verdict)),
    })
    .await
    .map_err(|e| e.to_string())
}

/// Shell-initiated shift begin. Shared with the sim console — the same real
/// path the supervisor assignment will use.
#[tauri::command]
pub async fn apply_batch(
    id: String,
    webview: Webview,
    modes: State<'_, Modes>,
    core: State<'_, CoreHandle>,
) -> Result<(), String> {
    ensure_conformance(&webview, &modes).or(ensure_sim(&webview, &modes))?;
    core.apply_batch(id).await.map_err(|e| e.to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConformanceResult {
    pub name: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Set once the report lands — the 60s no-report watchdog checks it.
pub struct ReportReceived(pub Arc<AtomicBool>);

#[tauri::command]
pub fn conformance_report(
    results: Vec<ConformanceResult>,
    webview: Webview,
    modes: State<'_, Modes>,
    received: State<'_, ReportReceived>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    ensure_conformance(&webview, &modes)?;
    received.0.store(true, Ordering::SeqCst);

    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut skipped = 0u32;
    println!("\nstation-shell conformance — real webview, real backend");
    for r in &results {
        let tag = match r.status.as_str() {
            "passed" => {
                passed += 1;
                "PASS"
            }
            "failed" => {
                failed += 1;
                "FAIL"
            }
            _ => {
                skipped += 1;
                "SKIP"
            }
        };
        println!("  {tag}  {}", r.name);
        if let Some(detail) = &r.detail {
            if r.status != "passed" {
                println!("        {detail}");
            }
        }
    }
    let gate_pass = failed == 0 && skipped == 0;
    println!("  {passed} passed, {failed} failed, {skipped} skipped");
    if skipped > 0 {
        println!("  GATE: FAIL (skips count as failures at the gate — wire both triggers)");
    } else if failed > 0 {
        println!("  GATE: FAIL");
    } else {
        println!("  GATE: PASS");
    }
    tracing::info!(
        "conformance report: {passed} passed / {failed} failed / {skipped} skipped -> {}",
        if gate_pass { "PASS" } else { "FAIL" }
    );
    if let Ok(json) = serde_json::to_string_pretty(&results) {
        let _ = std::fs::write(crate::paths::data_dir().join("conformance-results.json"), json);
    }
    app.exit(if gate_pass { 0 } else { 1 });
    Ok(())
}
