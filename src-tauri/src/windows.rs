//! Window creation: kiosk posture, the injected init script, WebView2
//! settings, and the ProcessFailed subscription. Exactly ONE window ever
//! carries the bridge (label "main"); the sim console gets no init script.

use crate::boot::BootCache;
use crate::cli::Cli;
use crate::watchdog::WatchdogEvent;
use std::sync::Arc;
use tauri::{AppHandle, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use tokio::sync::mpsc::UnboundedSender;

/// TAURI.md's three launch args, merged with wry's own defaults —
/// `additional_browser_args` REPLACES them, and duplicate --disable-features
/// flags override each other, so everything rides in one flag. Identical for
/// every window (they share one WebView2 environment).
pub const BROWSER_ARGS: &str = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection,CalculateNativeWinOcclusion --autoplay-policy=no-user-gesture-required --force-device-scale-factor=1";

pub const INIT_TEMPLATE: &str = include_str!("../gen/init_script.js");

/// Bake the CURRENT boot payload into the init script template. This baked
/// copy is only the FALLBACK — the script's primary path is the synchronous
/// station://boot pull, which is fresh on every document creation. The bake
/// is re-rendered whenever the watchdog recreates the window.
pub fn render_init_script(boot: &BootCache) -> String {
    let literal = serde_json::to_string(boot.load().as_str()).expect("escape boot json");
    INIT_TEMPLATE.replace("\"__BAKED_BOOT_JSON__\"", &literal)
}

/// Everything needed to (re)create the main window — the watchdog uses this
/// after a browser-process failure.
pub struct WindowFactory {
    pub cli: Cli,
    pub boot: Arc<BootCache>,
    pub watchdog_tx: UnboundedSender<WatchdogEvent>,
}

impl WindowFactory {
    pub fn create_main(&self, app: &AppHandle) -> tauri::Result<WebviewWindow> {
        let url = if self.cli.conformance {
            "harness/conformance.html?harness=1"
        } else {
            "index.html?shell=1"
        };
        let mut builder = WebviewWindowBuilder::new(app, "main", WebviewUrl::App(url.into()))
            .title("Station")
            .initialization_script(render_init_script(&self.boot))
            .additional_browser_args(BROWSER_ARGS)
            .use_https_scheme(false)
            .disable_drag_drop_handler()
            .focused(true);
        builder = if self.cli.kiosk() {
            builder
                .fullscreen(true)
                .decorations(false)
                .always_on_top(true)
                .skip_taskbar(true)
                .resizable(false)
        } else {
            builder.inner_size(1280.0, 720.0)
        };
        let window = builder.build()?;
        apply_webview2_settings(&window, self.watchdog_tx.clone());
        Ok(window)
    }

    /// The persistent fault screen (post-recovery-cap). No init script, no
    /// bridge, no scripts on the page at all — nothing left to fail.
    pub fn create_fault(&self, app: &AppHandle) -> tauri::Result<WebviewWindow> {
        let mut builder = WebviewWindowBuilder::new(
            app,
            "fault",
            WebviewUrl::External("http://station.localhost/fault".parse().expect("static url")),
        )
        .title("Station")
        .additional_browser_args(BROWSER_ARGS)
        .use_https_scheme(false)
        .focused(true);
        builder = if self.cli.kiosk() {
            builder.fullscreen(true).decorations(false).always_on_top(true).resizable(false)
        } else {
            builder.inner_size(1280.0, 720.0)
        };
        builder.build()
    }

    pub fn create_sim(&self, app: &AppHandle) -> tauri::Result<WebviewWindow> {
        WebviewWindowBuilder::new(app, "sim", WebviewUrl::App("sim/sim.html".into()))
            .title("Station Sim Console")
            .additional_browser_args(BROWSER_ARGS)
            .use_https_scheme(false)
            .inner_size(430.0, 720.0)
            .always_on_top(true)
            .build()
    }
}

/// TAURI.md's six WebView2 settings + the ProcessFailed watchdog hook.
/// The closure runs on the main thread; the event handler must never block —
/// it forwards to the watchdog channel and returns.
#[cfg(windows)]
fn apply_webview2_settings(window: &WebviewWindow, watchdog_tx: UnboundedSender<WatchdogEvent>) {
    let result = window.with_webview(move |pw| unsafe {
        use webview2_com::Microsoft::Web::WebView2::Win32::{
            ICoreWebView2, ICoreWebView2Settings6, COREWEBVIEW2_PROCESS_FAILED_KIND,
        };
        use webview2_com::ProcessFailedEventHandler;
        use windows::core::Interface;

        let controller = pw.controller();
        let core: ICoreWebView2 = match controller.CoreWebView2() {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("CoreWebView2 unavailable: {e}");
                return;
            }
        };
        match core.Settings().and_then(|s| s.cast::<ICoreWebView2Settings6>()) {
            Ok(s) => {
                let apply = || -> windows::core::Result<()> {
                    s.SetIsZoomControlEnabled(false.into())?; // base
                    s.SetAreDefaultContextMenusEnabled(false.into())?; // base
                    s.SetIsStatusBarEnabled(false.into())?; // base
                    s.SetAreBrowserAcceleratorKeysEnabled(false.into())?; // Settings3
                    s.SetIsPinchZoomEnabled(false.into())?; // Settings5
                    s.SetIsSwipeNavigationEnabled(false.into())?; // Settings6
                    Ok(())
                };
                if let Err(e) = apply() {
                    tracing::error!("WebView2 settings failed: {e}");
                } else {
                    tracing::info!("WebView2 kiosk settings applied");
                }
            }
            Err(e) => tracing::error!("WebView2 Settings6 cast failed: {e}"),
        }

        // Type inferred from add_ProcessFailed's signature — avoids pinning
        // the EventRegistrationToken import path across windows-rs versions.
        let mut token = std::mem::zeroed();
        let tx = watchdog_tx.clone();
        let handler = ProcessFailedEventHandler::create(Box::new(move |_core, args| {
            let mut kind = COREWEBVIEW2_PROCESS_FAILED_KIND::default();
            if let Some(args) = args {
                let _ = args.ProcessFailedKind(&mut kind);
            }
            let _ = tx.send(WatchdogEvent::ProcessFailed(kind.0));
            Ok(())
        }));
        if let Err(e) = core.add_ProcessFailed(&handler, &mut token) {
            tracing::error!("add_ProcessFailed failed: {e}");
        }
    });
    if let Err(e) = result {
        tracing::error!("with_webview failed: {e}");
    }
}

#[cfg(not(windows))]
fn apply_webview2_settings(_window: &WebviewWindow, _watchdog_tx: UnboundedSender<WatchdogEvent>) {}

/// COM Reload on the ICoreWebView2 — works even when the renderer process is
/// gone (a JS location.reload() obviously cannot run in a dead renderer).
#[cfg(windows)]
pub fn reload_via_com(window: &WebviewWindow) {
    let r = window.with_webview(|pw| unsafe {
        if let Ok(core) = pw.controller().CoreWebView2() {
            if let Err(e) = core.Reload() {
                tracing::error!("COM Reload failed: {e}");
            }
        }
    });
    if let Err(e) = r {
        tracing::error!("with_webview for reload failed: {e}");
    }
}

#[cfg(not(windows))]
pub fn reload_via_com(_window: &WebviewWindow) {}
