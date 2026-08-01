//! THE BATCH/RECORD SEAM.
//!
//! The guarded database write lives in the governed repository. This trait is
//! what the shell calls for batch data and per-tap outcome records; the sim
//! implementation is a JSON fixture plus a crash-safe local progress overlay.

use crate::contract::{BatchInfo, TapVerdict};
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

#[async_trait::async_trait]
pub trait BatchSource: Send + Sync {
    async fn list(&self) -> Result<Vec<BatchInfo>>;
    async fn get(&self, id: &str) -> Result<Option<BatchInfo>>;
    /// Records one tap outcome against a batch and returns the updated batch.
    /// Only success/dead reach here — retry is not a verdict.
    async fn record_outcome(&self, batch_id: &str, verdict: TapVerdict) -> Result<BatchInfo>;
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
struct Progress {
    completed: u32,
    failed: u32,
}

pub struct SimBatchSource {
    fixtures: Vec<BatchInfo>,
    overlay: Mutex<HashMap<String, Progress>>,
    overlay_path: Option<PathBuf>,
}

impl SimBatchSource {
    /// `overlay_path: None` = purely in-memory (tests, conformance).
    pub fn new(fixtures: Vec<BatchInfo>, overlay_path: Option<PathBuf>) -> Self {
        let overlay = overlay_path
            .as_deref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self { fixtures, overlay: Mutex::new(overlay), overlay_path }
    }

    pub fn from_fixture_json(json: &str, overlay_path: Option<PathBuf>) -> Result<Self> {
        let fixtures: Vec<BatchInfo> = serde_json::from_str(json).context("parse batch fixtures")?;
        Ok(Self::new(fixtures, overlay_path))
    }

    /// Conformance reset: drop all recorded progress.
    pub fn reset(&self) {
        self.overlay.lock().expect("overlay lock").clear();
        if let Some(path) = &self.overlay_path {
            let _ = std::fs::remove_file(path);
        }
    }

    fn merged(&self, fixture: &BatchInfo) -> BatchInfo {
        let overlay = self.overlay.lock().expect("overlay lock");
        let p = overlay.get(&fixture.id).copied().unwrap_or_default();
        BatchInfo {
            completed: fixture.completed + p.completed,
            failed: fixture.failed + p.failed,
            ..fixture.clone()
        }
    }

    fn persist_overlay(&self) {
        let Some(path) = &self.overlay_path else { return };
        let overlay = self.overlay.lock().expect("overlay lock");
        if let Ok(json) = serde_json::to_string_pretty(&*overlay) {
            let _ = crate::persist::session::atomic_write(path, json.as_bytes());
        }
    }
}

#[async_trait::async_trait]
impl BatchSource for SimBatchSource {
    async fn list(&self) -> Result<Vec<BatchInfo>> {
        Ok(self.fixtures.iter().map(|f| self.merged(f)).collect())
    }

    async fn get(&self, id: &str) -> Result<Option<BatchInfo>> {
        Ok(self.fixtures.iter().find(|f| f.id == id).map(|f| self.merged(f)))
    }

    async fn record_outcome(&self, batch_id: &str, verdict: TapVerdict) -> Result<BatchInfo> {
        let fixture = self
            .fixtures
            .iter()
            .find(|f| f.id == batch_id)
            .ok_or_else(|| anyhow!("unknown batch: {batch_id}"))?;
        {
            let mut overlay = self.overlay.lock().expect("overlay lock");
            let p = overlay.entry(batch_id.to_string()).or_default();
            match verdict {
                TapVerdict::Success => p.completed += 1,
                TapVerdict::Dead => p.failed += 1,
                TapVerdict::Retry => {}
            }
        }
        self.persist_overlay();
        Ok(self.merged(fixture))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source() -> SimBatchSource {
        SimBatchSource::from_fixture_json(
            r#"[{"id":"b1","name":"B ONE","total":10,"completed":4,"failed":1}]"#,
            None,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn record_success_increments_completed() {
        let s = source();
        let updated = s.record_outcome("b1", TapVerdict::Success).await.unwrap();
        assert_eq!(updated.completed, 5);
        assert_eq!(updated.failed, 1);
    }

    #[tokio::test]
    async fn record_dead_increments_failed() {
        let s = source();
        let updated = s.record_outcome("b1", TapVerdict::Dead).await.unwrap();
        assert_eq!(updated.failed, 2);
    }

    #[tokio::test]
    async fn unknown_batch_rejects() {
        let s = source();
        assert!(s.record_outcome("nope", TapVerdict::Success).await.is_err());
    }

    #[tokio::test]
    async fn list_returns_fresh_merged_copies() {
        let s = source();
        s.record_outcome("b1", TapVerdict::Success).await.unwrap();
        let a = s.list().await.unwrap();
        let b = s.list().await.unwrap();
        assert_eq!(a, b);
        assert_eq!(a[0].completed, 5);
    }
}
