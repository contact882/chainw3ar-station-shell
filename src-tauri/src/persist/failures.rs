//! Failure RECORDS (not the retry counter — that is session-scoped by
//! design). Append-only JSONL consumed later by the supervisor
//! failure-review module. Log-only data: chip refs and failure classes never
//! reach an operator surface.

use serde::Serialize;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FailureRecord<'a> {
    pub ts: u64,
    pub chip_ref: &'a str,
    pub class: &'a str,
    pub verdict: &'a str,
    /// 1-based attempt number for this chip within the session.
    pub attempt: u8,
    /// True when this record is the 3rd-consecutive-failure escalation.
    pub escalated: bool,
    pub reader_id: &'a str,
    pub batch_id: &'a str,
    pub shift_id: &'a str,
}

/// An overrun success: a chip keyed against a batch already at/past total.
/// Distinct file from failures.jsonl — it is not a failure, it is a
/// licensing/inventory event the supervisor module must be able to answer
/// exactly ("why 525/500?"). Log-internal; never operator-visible.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverrunRecord<'a> {
    pub ts: u64,
    pub chip_ref: &'a str,
    pub reader_id: &'a str,
    pub batch_id: &'a str,
    pub shift_id: &'a str,
    pub completed: u32,
    pub total: u32,
    /// completed - total after this success (1 for the first overrun unit).
    pub past_total: u32,
}

pub struct JsonlLog {
    path: PathBuf,
    label: &'static str,
}

impl JsonlLog {
    pub fn new(path: PathBuf, label: &'static str) -> Self {
        Self { path, label }
    }

    pub fn append<T: Serialize>(&self, record: &T) {
        let write = || -> anyhow::Result<()> {
            if let Some(parent) = self.path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut f = OpenOptions::new().create(true).append(true).open(&self.path)?;
            serde_json::to_writer(&mut f, record)?;
            f.write_all(b"\n")?;
            f.flush()?;
            Ok(())
        };
        if let Err(e) = write() {
            tracing::error!("{} record append failed: {e}", self.label);
        }
    }
}

pub struct FailureLog(JsonlLog);

impl FailureLog {
    pub fn new(path: PathBuf) -> Self {
        Self(JsonlLog::new(path, "failure"))
    }

    pub fn append(&self, record: &FailureRecord<'_>) {
        self.0.append(record);
    }
}

pub struct OverrunLog(JsonlLog);

impl OverrunLog {
    pub fn new(path: PathBuf) -> Self {
        Self(JsonlLog::new(path, "overrun"))
    }

    pub fn append(&self, record: &OverrunRecord<'_>) {
        self.0.append(record);
    }
}
