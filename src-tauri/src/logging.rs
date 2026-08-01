//! Shell logs: daily-rolling file + stderr. UI console output arrives via the
//! `log_console` command and lands under the `ui_console` target —
//! `station-ui fault` lines at ERROR. Confidentiality note: chip refs and
//! failure classes appear ONLY in these files, never on an operator surface.

use std::path::Path;
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

pub struct LogGuard {
    _file_guard: tracing_appender::non_blocking::WorkerGuard,
}

/// Retention sweep: shell.log.* older than `keep_days` are deleted at
/// startup. Lesson from 2026-08-01: one logging loop wrote 23 MB in ~90s —
/// unbounded logs on a floor machine are a disk-exhaustion path, not a
/// hypothetical. failures/overruns JSONL are NOT swept (supervisor feed).
pub fn sweep_old_logs(log_dir: &Path, keep_days: u64) {
    let cutoff = std::time::SystemTime::now()
        - std::time::Duration::from_secs(keep_days * 24 * 60 * 60);
    let Ok(entries) = std::fs::read_dir(log_dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with("shell.log") {
            continue;
        }
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .map(|t| t < cutoff)
            .unwrap_or(false);
        if old {
            match std::fs::remove_file(entry.path()) {
                Ok(()) => eprintln!("log retention: removed {}", name.to_string_lossy()),
                Err(e) => eprintln!("log retention: could not remove {}: {e}", name.to_string_lossy()),
            }
        }
    }
}

pub fn init(log_dir: &Path) -> anyhow::Result<LogGuard> {
    std::fs::create_dir_all(log_dir)?;
    sweep_old_logs(log_dir, 14);
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

#[cfg(test)]
mod tests {
    use super::sweep_old_logs;
    use std::fs::{self, FileTimes, OpenOptions};
    use std::time::{Duration, SystemTime};

    /// S9: fails if the retention sweep is removed or stops matching.
    #[test]
    fn sweep_removes_old_shell_logs_and_keeps_everything_else() {
        let dir = std::env::temp_dir().join(format!("station-logs-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let old_log = dir.join("shell.log.2026-01-01");
        let new_log = dir.join("shell.log.2026-08-01");
        let unrelated = dir.join("failures.jsonl");
        for p in [&old_log, &new_log, &unrelated] {
            fs::write(p, b"x").unwrap();
        }
        let ancient = SystemTime::now() - Duration::from_secs(20 * 24 * 60 * 60);
        OpenOptions::new()
            .write(true)
            .open(&old_log)
            .unwrap()
            .set_times(FileTimes::new().set_modified(ancient))
            .unwrap();

        sweep_old_logs(&dir, 14);

        assert!(!old_log.exists(), "old shell.log must be swept");
        assert!(new_log.exists(), "recent shell.log must be kept");
        assert!(unrelated.exists(), "supervisor-feed JSONL is never swept");
        fs::remove_dir_all(&dir).ok();
    }
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
