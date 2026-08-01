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

pub struct FailureLog {
    path: PathBuf,
}

impl FailureLog {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn append(&self, record: &FailureRecord<'_>) {
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
            tracing::error!("failure record append failed: {e}");
        }
    }
}
