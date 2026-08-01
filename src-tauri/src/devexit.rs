//! --dev-exit machinery: the OS exit chord, its arm-time delivery canary,
//! and the guided physical probe (`--exit-probe`).
//!
//! Incident 2026-08-01 (#2): an armed chord failed in the field and left zero
//! evidence — the old "armed" log line attested OS *registration* only, while
//! delivery (input chain → WM_HOTKEY → event-loop pump → handler) had no
//! witness. Arming is therefore a PROOF: a synthetic press of the REAL chord
//! is driven through the full OS chain before the launch is trusted. If it
//! does not come back, the launch REFUSES to continue (exit 71) — a dev exit
//! that reports success without working is worse than one that refuses to
//! start (founder decision D2).
//!
//! HARDWARE FINDING (2026-08-01, --exit-probe on the reference machine): the
//! original 4-key chord Ctrl+Alt+Shift+Q could not be formed by the physical
//! keyboard (membrane rollover limit) — probe step 1 (synthetic) passed,
//! step 2 (physical) failed. The chord is now Ctrl+Shift+F12 HELD for 2
//! seconds: two modifiers + one key is the most rollover-safe chord shape
//! (the standard app-shortcut shape membrane matrices are built for), no Alt
//! avoids AltGr collisions on international layouts, and the hold plus a
//! fire-time physical key-state re-check replaces the accidental-press
//! resistance the fourth key used to provide. Do not reintroduce a 4-key
//! chord — this is the incident that answers why.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::AppHandle;
use tauri_plugin_global_shortcut::{Code, Modifiers, Shortcut, ShortcutState};

/// Process exit code for "dev exit requested but its delivery could not be
/// verified". Distinct from 70 (fault window unusable).
pub const EXIT_UNVERIFIED: i32 = 71;

/// How long the chord must be held, continuously, to fire.
const HOLD_MS: u64 = 2000;

pub fn chord() -> Shortcut {
    Shortcut::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::F12)
}

pub struct DevExitFlags {
    /// Any Pressed event of the chord reached the handler — the delivery
    /// proof. The arm-time canary is a synthetic TAP of the real chord: the
    /// sub-hold duration guarantees no exit, so what is verified is exactly
    /// what a human later presses.
    pub delivery_seen: Arc<AtomicBool>,
    /// A full 2s hold completed (probe mode reports instead of exiting).
    pub chord_held: Arc<AtomicBool>,
}

struct Hold {
    gen: AtomicU64,
    /// Some(generation) while the chord is down; None once released.
    pressed: Mutex<Option<u64>>,
}

/// Register the chord and wire the hold logic. A registration failure (e.g.
/// another process owns the chord) propagates and aborts the launch — it can
/// never be silent.
pub fn install(
    handle: &AppHandle,
    probe: bool,
) -> Result<DevExitFlags, Box<dyn std::error::Error>> {
    let chord = chord();
    let delivery_seen = Arc::new(AtomicBool::new(false));
    let chord_held = Arc::new(AtomicBool::new(false));
    let hold = Arc::new(Hold { gen: AtomicU64::new(0), pressed: Mutex::new(None) });
    let (seen, held) = (delivery_seen.clone(), chord_held.clone());
    handle.plugin(
        tauri_plugin_global_shortcut::Builder::new()
            .with_shortcuts([chord])?
            .with_handler(move |app_handle, shortcut, event| {
                if shortcut != &chord {
                    return;
                }
                match event.state() {
                    ShortcutState::Pressed => {
                        seen.store(true, Ordering::SeqCst);
                        let mut pressed = hold.pressed.lock().unwrap();
                        if pressed.is_some() {
                            return; // key-repeat while held — keep the original deadline
                        }
                        let gen = hold.gen.fetch_add(1, Ordering::SeqCst) + 1;
                        *pressed = Some(gen);
                        drop(pressed);
                        let hold = hold.clone();
                        let held = held.clone();
                        let app = app_handle.clone();
                        tauri::async_runtime::spawn(async move {
                            tokio::time::sleep(Duration::from_millis(HOLD_MS)).await;
                            // Fire only if THIS press is still down, and the
                            // keys are PHYSICALLY down right now — a lost
                            // release event or a transient ghost can never
                            // fire the exit.
                            if *hold.pressed.lock().unwrap() != Some(gen) || !chord_keys_down() {
                                return;
                            }
                            if probe {
                                held.store(true, Ordering::SeqCst);
                            } else {
                                tracing::info!(
                                    "dev exit chord (Ctrl+Shift+F12 held 2s) — shutting down cleanly"
                                );
                                app.exit(0);
                            }
                        });
                    }
                    ShortcutState::Released => {
                        *hold.pressed.lock().unwrap() = None;
                    }
                }
            })
            .build(),
    )?;
    Ok(DevExitFlags { delivery_seen, chord_held })
}

/// Drive a synthetic TAP of the real chord through the OS and wait for its
/// Pressed event to come back through the event-loop pump and the handler.
/// TRUE means every software layer between "registered" and "the handler
/// ran" works right now, for the exact chord a human will press. The tap is
/// far below the 2s hold, so it can never exit.
pub async fn verify_delivery(delivery_seen: Arc<AtomicBool>) -> bool {
    // Let the event loop settle before injecting.
    tokio::time::sleep(Duration::from_millis(300)).await;
    inject_chord_tap();
    for _ in 0..40 {
        if delivery_seen.load(Ordering::SeqCst) {
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
        if verify_delivery(flags.delivery_seen).await {
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

/// `--exit-probe` driver: step 1 proves the software chain (synthetic tap of
/// the real chord), step 2 requires a PHYSICAL 2s hold within 15s — the only
/// test that can catch a keyboard unable to form the chord (this caught the
/// 4-key chord's rollover failure on 2026-08-01). Exit code is the verdict.
/// Console output is visible under `cargo run`; the release exe is
/// windows-subsystem and prints nothing — use a debug build or read the log.
pub fn spawn_probe(handle: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let flags = install(handle, true)?;
    let app = handle.clone();
    tauri::async_runtime::spawn(async move {
        println!("EXIT PROBE — step 1/2: synthetic delivery check…");
        if !verify_delivery(flags.delivery_seen).await {
            println!("EXIT PROBE: FAIL — the synthetic chord tap never reached the handler.");
            println!("The hotkey pipeline is broken at the software layer on this machine.");
            tracing::error!("exit probe: synthetic delivery FAILED");
            app.exit(1);
            return;
        }
        println!("step 1 OK — every software layer delivers.");
        println!("step 2/2: on the KEYBOARD, press AND HOLD Ctrl+Shift+F12 for 2 full");
        println!("seconds, starting within the next 15 seconds.");
        println!("KEYBOARD ONLY — the card reader plays no part in this probe; tapping");
        println!("a card does nothing here.");
        println!("(this proves the physical keyboard can form and sustain the chord)");
        tracing::info!("exit probe: synthetic delivery ok; awaiting physical 2s hold");
        for _ in 0..175 {
            if flags.chord_held.load(Ordering::SeqCst) {
                println!("EXIT PROBE: PASS — physical 2s hold received end-to-end.");
                tracing::info!("exit probe: PASS (physical chord hold received)");
                app.exit(0);
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        println!("EXIT PROBE: FAIL — no completed 2s hold within the window.");
        println!("Synthetic delivery works, so the software chain is fine: either this");
        println!("keyboard cannot form/sustain Ctrl+Shift+F12 (rollover/ghosting) or");
        println!("something intercepts the physical press. Try another keyboard.");
        tracing::warn!("exit probe: FAIL (no physical chord hold within window)");
        app.exit(1);
    });
    Ok(())
}

/// Are Ctrl, Shift and F12 all PHYSICALLY down at this instant?
#[cfg(windows)]
fn chord_keys_down() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, VK_CONTROL, VK_F12, VK_SHIFT,
    };
    unsafe {
        [VK_CONTROL, VK_SHIFT, VK_F12]
            .iter()
            .all(|vk| (GetAsyncKeyState(vk.0 as i32) as u16) & 0x8000 != 0)
    }
}

#[cfg(not(windows))]
fn chord_keys_down() -> bool {
    true
}

#[cfg(windows)]
fn inject_chord_tap() {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS,
        KEYEVENTF_KEYUP, VIRTUAL_KEY, VK_CONTROL, VK_F12, VK_SHIFT,
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
        key(VK_SHIFT, false),
        key(VK_F12, false),
        key(VK_F12, true),
        key(VK_SHIFT, true),
        key(VK_CONTROL, true),
    ];
    let sent = unsafe { SendInput(&seq, std::mem::size_of::<INPUT>() as i32) };
    if sent != seq.len() as u32 {
        tracing::warn!("canary injection incomplete ({sent}/{} events)", seq.len());
    }
}

#[cfg(not(windows))]
fn inject_chord_tap() {}
