// Chainw3ar encoding station shell. Hosts the frozen operator UI, injects the
// StationBridge, owns verdicts/retries/persistence. Keying and the guarded DB
// live in a separate governed repo — this build drives SIMULATED seams only.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod boot;
mod cli;
mod commands;
mod config;
mod contract;
mod core;
mod logging;
mod paths;
mod persist;
mod probe;
mod push;
mod reader;
mod seams;
mod watchdog;
mod windows;

use crate::boot::BootCache;
use crate::commands::conformance::ReportReceived;
use crate::commands::ui::Heartbeat;
use crate::commands::Modes;
use crate::contract::StationUiConfig;
use crate::core::actor::{CoreDeps, Timing};
use crate::core::state::StationState;
use crate::persist::failures::FailureLog;
use crate::persist::session::SessionStore;
use crate::push::EvalPusher;
use crate::seams::batch_source::{BatchSource, SimBatchSource};
use crate::seams::keying::{KeyingService, SimKeying, SimPolicy};
use crate::windows::WindowFactory;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::Manager;

const EMBEDDED_FIXTURES: &str = include_str!("../fixtures/batches.json");

fn load_fixture_json() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.join("fixtures/batches.json")))
        .filter(|p| p.exists())
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_else(|| EMBEDDED_FIXTURES.to_string())
}

fn main() {
    let cli = cli::Cli::parse(std::env::args().skip(1));
    let _log_guard = logging::init(&paths::log_dir()).expect("logging init");
    tracing::info!("station-shell starting: {cli:?}");

    let station_config = config::StationConfig::load(cli.config.as_deref());

    if cli.reader_probe {
        // Hardware gate: judge the detection path against the real reader,
        // no windows involved. Exits before any Tauri machinery.
        std::process::exit(probe::run(station_config.reader_match.clone()));
    }

    let ui_config = StationUiConfig { fail_bin_side: station_config.fail_bin_side };
    let data_dir = paths::data_dir();

    // Conformance runs against its own session file and an in-memory batch
    // overlay — the gate must never clobber a real shift on this machine.
    let session = SessionStore::new(data_dir.join(if cli.conformance {
        "conformance-session.json"
    } else {
        "session.json"
    }));
    if cli.conformance {
        let _ = session.reset();
    }
    let persisted = if cli.conformance { None } else { session.load() };
    if persisted.as_ref().and_then(|p| p.shift.as_ref()).is_some() {
        tracing::info!("crash/restart recovery: resuming persisted shift");
    }
    let state = StationState::new(ui_config, persisted);

    let sim_batches = Arc::new(
        SimBatchSource::from_fixture_json(
            &load_fixture_json(),
            (!cli.conformance).then(|| data_dir.join("sim_progress.json")),
        )
        .expect("batch fixtures parse"),
    );
    let sim_keying = Arc::new(SimKeying::new(SimPolicy {
        weight_keyed: station_config.sim.weight_keyed,
        weight_recoverable: station_config.sim.weight_recoverable,
        weight_permanent: station_config.sim.weight_permanent,
        latency_ms: if cli.conformance { 0 } else { station_config.sim.latency_ms },
        ..SimPolicy::default()
    }));

    let boot_cache = Arc::new(BootCache::new(
        &state.boot_payload(station_config.strings.clone()),
    ));
    let (core_handle, core_rx) = core::actor::channel();
    let (watchdog_tx, watchdog_rx) = tokio::sync::mpsc::unbounded_channel();
    let heartbeat = Heartbeat::new();
    let modes = Modes { sim: cli.sim, conformance: cli.conformance };
    let report_received = Arc::new(AtomicBool::new(false));
    let kiosk = cli.kiosk();

    let protocol_boot = boot_cache.clone();
    let protocol_core = core_handle.clone();
    let deps_seed = Some((state, session));

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_focus();
            }
        }))
        .register_uri_scheme_protocol("station", move |_ctx, request| {
            match request.uri().path() {
                "/boot" => {
                    // Must stay microseconds-cheap: it answers a SYNCHRONOUS
                    // XHR while the renderer's JS thread blocks. ArcSwap only.
                    protocol_core.try_notify_boot_pulled();
                    boot::boot_response(&protocol_boot)
                }
                "/fault" => boot::fault_page(),
                _ => boot::not_found(),
            }
        })
        .manage(core_handle.clone())
        .manage(modes)
        .manage(sim_keying.clone())
        .manage(boot_cache.clone())
        .manage(heartbeat.clone())
        .manage(ReportReceived(report_received.clone()))
        .invoke_handler(tauri::generate_handler![
            commands::ui::list_batches,
            commands::ui::select_batch,
            commands::ui::end_shift,
            commands::ui::bridge_attached,
            commands::ui::log_console,
            commands::ui::heartbeat,
            commands::ui::boot_snapshot,
            commands::sim::sim_present_chip,
            commands::sim::sim_set_reader,
            commands::sim::sim_set_downstream,
            commands::sim::sim_blip,
            commands::sim::sim_swap_batch,
            commands::sim::sim_end_shift,
            commands::sim::sim_arm_policy,
            commands::sim::sim_get_state,
            commands::sim::sim_quit,
            commands::conformance::conformance_reset,
            commands::conformance::conformance_fire_tap,
            commands::conformance::apply_batch,
            commands::conformance::conformance_report,
        ])
        .on_window_event(move |window, event| {
            if kiosk && window.label() == "main" {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    // Kiosk: the exe stops via service control, not Alt+F4.
                    api.prevent_close();
                }
            }
        })
        .setup(move |app| {
            let app_handle = app.handle().clone();
            let (state, session) = deps_seed.expect("deps seed present");

            let deps = CoreDeps {
                state,
                keying: sim_keying.clone() as Arc<dyn KeyingService>,
                batch_source: sim_batches.clone() as Arc<dyn BatchSource>,
                sim_keying: Some(sim_keying.clone()),
                sim_batches: Some(sim_batches.clone()),
                session,
                failures: FailureLog::new(paths::data_dir().join("failures.jsonl")),
                boot_cache: boot_cache.clone(),
                pusher: Arc::new(EvalPusher::new(app_handle.clone())),
                strings: station_config.strings.clone(),
                timing: Timing::default(),
                conformance: cli.conformance,
            };
            tauri::async_runtime::spawn(core::actor::run(core_rx, deps));

            app.manage(WindowFactory {
                cli: cli.clone(),
                boot: boot_cache.clone(),
                watchdog_tx: watchdog_tx.clone(),
            });
            app.state::<WindowFactory>().create_main(&app_handle)?;
            if cli.sim {
                app.state::<WindowFactory>().create_sim(&app_handle)?;
            }

            if let Some(cycles) = cli.spike {
                // Reload-freshness spike: mutate state, reload the document
                // via COM, and demand the fresh document boot-pulled CURRENT
                // truth every single time (stale_attaches must stay 0).
                let core = core_handle.clone();
                let handle = app_handle.clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
                    let ids = ["b-tee-navy", "b-hoodie"];
                    for i in 0..cycles {
                        let id = ids[(i as usize) % ids.len()].to_string();
                        if let Err(e) = core.apply_batch(id).await {
                            eprintln!("spike: apply_batch failed: {e}");
                            handle.exit(1);
                            return;
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                        if let Some(window) = handle.get_webview_window("main") {
                            crate::windows::reload_via_com(&window);
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(900)).await;
                    }
                    match core.sim_state().await {
                        Ok(view) => {
                            println!(
                                "spike: {cycles} mutate+reload cycles, seq {}, stale boots {}",
                                view.seq, view.stale_attaches
                            );
                            println!(
                                "SPIKE: {}",
                                if view.stale_attaches == 0 { "PASS" } else { "FAIL" }
                            );
                            handle.exit(if view.stale_attaches == 0 { 0 } else { 1 });
                        }
                        Err(e) => {
                            eprintln!("spike: state read failed: {e}");
                            handle.exit(1);
                        }
                    }
                });
            }

            if cli.dev_exit {
                // Dev escape hatch: OS-level global shortcut, deliberately NOT
                // a page listener — it must work even when the renderer is
                // hung or black, which is exactly when an exit is needed.
                // Registered ONLY under --dev-exit; production launches never
                // contain it. See README "Exiting kiosk mode".
                use tauri_plugin_global_shortcut::{Code, Modifiers, Shortcut, ShortcutState};
                let chord = Shortcut::new(
                    Some(Modifiers::CONTROL | Modifiers::ALT | Modifiers::SHIFT),
                    Code::KeyQ,
                );
                app.handle().plugin(
                    tauri_plugin_global_shortcut::Builder::new()
                        .with_shortcuts([chord])?
                        .with_handler(move |app_handle, shortcut, event| {
                            if event.state() == ShortcutState::Pressed && shortcut == &chord {
                                tracing::info!("dev exit chord (Ctrl+Alt+Shift+Q) — shutting down cleanly");
                                app_handle.exit(0);
                            }
                        })
                        .build(),
                )?;
                tracing::info!("dev exit chord armed (--dev-exit)");
            }

            if cli.conformance {
                // No reader thread, no watchdog reloads, no spontaneous pushes
                // — but a hung harness must not hang the gate.
                let received = report_received.clone();
                let handle = app_handle.clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                    if !received.load(Ordering::SeqCst) {
                        eprintln!("conformance: no report within 60s — webview dead or harness hung");
                        handle.exit(2);
                    }
                });
            } else {
                reader::spawn(core_handle.clone(), station_config.reader_match.clone());
                watchdog::spawn(app_handle.clone(), watchdog_rx, heartbeat.clone());
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("station-shell run failed");
}
