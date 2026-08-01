pub mod conformance;
pub mod sim;
pub mod ui;

/// Which optional command families this launch enabled. Sim commands are
/// additionally window-label-gated: the kiosk webview ("main") can never
/// invoke them even if it tried.
#[derive(Debug, Clone, Copy)]
pub struct Modes {
    pub sim: bool,
    pub conformance: bool,
}

pub fn ensure_sim<R: tauri::Runtime>(
    webview: &tauri::Webview<R>,
    modes: &Modes,
) -> Result<(), String> {
    if modes.sim && webview.label() == "sim" {
        Ok(())
    } else {
        Err("not available".into())
    }
}

pub fn ensure_conformance<R: tauri::Runtime>(
    webview: &tauri::Webview<R>,
    modes: &Modes,
) -> Result<(), String> {
    if modes.conformance && webview.label() == "main" {
        Ok(())
    } else {
        Err("not available".into())
    }
}
