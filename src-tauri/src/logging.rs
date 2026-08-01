//! Shell logs: daily-rolling file + stderr. UI console output arrives via the
//! `log_console` command and lands under the `ui_console` target —
//! `station-ui fault` lines at ERROR. Confidentiality note: chip refs and
//! failure classes appear ONLY in these files, never on an operator surface.

use std::path::Path;
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

pub struct LogGuard {
    _file_guard: tracing_appender::non_blocking::WorkerGuard,
}

pub fn init(log_dir: &Path) -> anyhow::Result<LogGuard> {
    std::fs::create_dir_all(log_dir)?;
    let file_appender = tracing_appender::rolling::daily(log_dir, "shell.log");
    let (file_writer, file_guard) = tracing_appender::non_blocking(file_appender);

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_writer(file_writer).with_ansi(false))
        .with(fmt::layer().with_writer(std::io::stderr))
        .init();
    Ok(LogGuard { _file_guard: file_guard })
}

/// Forwarded page console line → shell log.
pub fn log_ui_console(level: &str, message: &str) {
    let fault = message.contains("station-ui fault");
    match (level, fault) {
        (_, true) | ("error", _) => tracing::error!(target: "ui_console", "{message}"),
        ("warn", _) => tracing::warn!(target: "ui_console", "{message}"),
        _ => tracing::info!(target: "ui_console", "{message}"),
    }
}
