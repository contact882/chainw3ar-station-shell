//! --dev-exit machinery: the OS exit chord, its arm-time delivery canary,
//! and the guided physical probe (`--exit-probe`).
//!
//! Incident 2026-08-01 (#2): an armed chord failed in the field and left zero
//! evidence — the old "armed" log line attested OS *registration* only, while
//! delivery (input chain → WM_HOTKEY → event-loop pump → handler) had no
//! witness. Arming is therefore now a PROOF: a canary hotkey is registered
//! alongside the chord and a synthetic press is driven through the full OS
//! chain before the launch is trusted. If the canary does not come back, the
//! launch REFUSES to continue (exit 71) — a dev exit that reports success
//! without working is worse than one that refuses to start (founder decision
//! D2). Honest limit: the canary proves every software layer; it cannot prove
//! a physical keyboard can form a 4-simultaneous-key chord — that is what
//! `--exit-probe` exists for, with a human at the keys.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tauri::AppHandle;
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

/// Process exit code for "dev exit requested but its delivery could not be
/// verified". Distinct from 70 (fault window unusable).
pub const EXIT_UNVERIFIED: i32 = 71;

fn mods() -> Modifiers {
    Modifiers::CONTROL | Modifiers::ALT | Modifiers::SHIFT
}

pub fn chord() -> Shortcut {
    Shortcut::new(Some(mods()), Code::KeyQ)
}

/// Same modifiers, a key no physical keyboard emits by accident — exists only
/// to round-trip a synthetic press through the real delivery chain.
fn canary() -> Shortcut {
    Shortcut::new(Some(mods()), Code::F24)
}

pub struct DevExitFlags {
    pub canary_seen: Arc<AtomicBool>,
    /// Probe mode only: a chord press sets this instead of exiting.
    pub chord_seen: Arc<AtomicBool>,
}

/// Register chord + canary. A registration failure (e.g. another process owns
/// the chord) propagates and aborts the launch — it can never be silent.
pub fn install(
    handle: &AppHandle,
    probe: bool,
) -> Result<DevExitFlags, Box<dyn std::error::Error>> {
    let chord = chord();
    let canary = canary();
    let canary_seen = Arc::new(AtomicBool::new(false));
    let chord_seen = Arc::new(AtomicBool::new(false));
    let (cs, ks) = (canary_seen.clone(), chord_seen.clone());
    handle.plugin(
        tauri_plugin_global_shortcut::Builder::new()
            .with_shortcuts([chord, canary])?
            .with_handler(move |app_handle, shortcut, event| {
                if event.state() != ShortcutState::Pressed {
                    return;
                }
                if shortcut == &canary {
                    cs.store(true, Ordering::SeqCst);
                } else if shortcut == &chord {
                    if probe {
                        ks.store(true, Ordering::SeqCst);
                    } else {
                        tracing::info!(
                            "dev exit chord (Ctrl+Alt+Shift+Q) — shutting down cleanly"
                        );
                        app_handle.exit(0);
                    }
                }
            })
            .build(),
    )?;
    Ok(DevExitFlags { canary_seen, chord_seen })
}

/// Drive a synthetic canary press through the OS and wait for it to come back
/// through the event-loop pump and the plugin handler. TRUE means every
/// software layer between "registered" and "the handler ran" works right now.
/// Unregisters the canary once verified.
pub async fn verify_delivery(app: AppHandle, canary_seen: Arc<AtomicBool>) -> bool {
    // Let the event loop settle before injecting.
    tokio::time::sleep(Duration::from_millis(300)).await;
    inject_canary();
    for _ in 0..40 {
        if canary_seen.load(Ordering::SeqCst) {
            if let Err(e) = app.global_shortcut().unregister(canary()) {
                tracing::warn!("canary unregister failed (harmless): {e}");
            }
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

/// Normal-launch arming: register, then verify delivery before trusting it.
pub fn arm_verified(handle: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let flags = install(handle, false)?;
    tracing::info!("dev exit chord registered (--dev-exit) — verifying delivery");
    let app = handle.clone();
    tauri::async_runtime::spawn(async move {
        if verify_delivery(app.clone(), flags.canary_seen).await {
            tracing::info!("dev exit chord armed (delivery verified)");
        } else {
            tracing::error!(
                "dev exit chord registered but the delivery canary never came back — \
                 the exit chord cannot be trusted; refusing to run (exit {EXIT_UNVERIFIED}). \
                 Run --exit-probe to diagnose."
            );
            app.exit(EXIT_UNVERIFIED);
        }
    });
    Ok(())
}

/// `--exit-probe` driver: step 1 proves the software chain (synthetic canary),
/// step 2 requires a PHYSICAL chord press within 15s — the only test that can
/// catch a keyboard unable to form the 4-key chord. Exit code is the verdict.
/// Console output is visible under `cargo run`; the release exe is
/// windows-subsystem and prints nothing — use a debug build or read the log.
pub fn spawn_probe(handle: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let flags = install(handle, true)?;
    let app = handle.clone();
    tauri::async_runtime::spawn(async move {
        println!("EXIT PROBE — step 1/2: synthetic delivery check…");
        if !verify_delivery(app.clone(), flags.canary_seen).await {
            println!("EXIT PROBE: FAIL — the synthetic canary never reached the handler.");
            println!("The hotkey pipeline is broken at the software layer on this machine.");
            tracing::error!("exit probe: synthetic delivery FAILED");
            app.exit(1);
            return;
        }
        println!("step 1 OK — every software layer delivers.");
        println!("step 2/2: press Ctrl+Alt+Shift+Q on the PHYSICAL keyboard within 15 seconds.");
        println!("(this proves the keyboard itself can form the 4-key chord)");
        tracing::info!("exit probe: synthetic delivery ok; awaiting physical chord");
        for _ in 0..150 {
            if flags.chord_seen.load(Ordering::SeqCst) {
                println!("EXIT PROBE: PASS — physical chord received end-to-end.");
                tracing::info!("exit probe: PASS (physical chord received)");
                app.exit(0);
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        println!("EXIT PROBE: FAIL — no physical chord within 15 seconds.");
        println!("Synthetic delivery works, so the software chain is fine: either this");
        println!("keyboard cannot form Ctrl+Alt+Shift+Q simultaneously (rollover/ghosting)");
        println!("or something intercepts the physical press. Try another keyboard.");
        tracing::warn!("exit probe: FAIL (no physical chord within 15s)");
        app.exit(1);
    });
    Ok(())
}

#[cfg(windows)]
fn inject_canary() {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS,
        KEYEVENTF_KEYUP, VIRTUAL_KEY, VK_CONTROL, VK_F24, VK_MENU, VK_SHIFT,
    };
    fn key(vk: VIRTUAL_KEY, up: bool) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: 0,
                    dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) },
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }
    let seq = [
        key(VK_CONTROL, false),
        key(VK_MENU, false),
        key(VK_SHIFT, false),
        key(VK_F24, false),
        key(VK_F24, true),
        key(VK_SHIFT, true),
        key(VK_MENU, true),
        key(VK_CONTROL, true),
    ];
    let sent = unsafe { SendInput(&seq, std::mem::size_of::<INPUT>() as i32) };
    if sent != seq.len() as u32 {
        tracing::warn!("canary injection incomplete ({sent}/{} events)", seq.len());
    }
}

#[cfg(not(windows))]
fn inject_canary() {}
