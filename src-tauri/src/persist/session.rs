//! Crash-safe session persistence. The recovery contract hinges on this file:
//! the write happens BEFORE any event leaves the process, atomically
//! (temp + sync_all + rename on the same volume), so a kill at any instant
//! leaves either the old state or the new state — never a torn file.

use crate::core::state::{PersistedSession, SESSION_SCHEMA};
use anyhow::{Context, Result};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("no parent dir")?;
    fs::create_dir_all(parent)?;
    let tmp = path.with_extension("tmp");
    {
        let mut f = File::create(&tmp).context("create temp")?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path).context("rename over")?;
    Ok(())
}

pub struct SessionStore {
    path: PathBuf,
}

impl SessionStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// A missing, unreadable, or schema-mismatched file is FRESH STATE, not an
    /// error — logged loudly, because losing a shift is operationally visible.
    pub fn load(&self) -> Option<PersistedSession> {
        let raw = match fs::read_to_string(&self.path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(e) => {
                tracing::error!("session file unreadable ({e}); starting fresh");
                return None;
            }
        };
        match serde_json::from_str::<PersistedSession>(&raw) {
            Ok(p) if p.schema == SESSION_SCHEMA => Some(p),
            Ok(p) => {
                tracing::error!("session schema {} != {SESSION_SCHEMA}; starting fresh", p.schema);
                None
            }
            Err(e) => {
                tracing::error!("session file corrupt ({e}); starting fresh");
                None
            }
        }
    }

    pub fn save(&self, session: &PersistedSession) -> Result<()> {
        atomic_write(&self.path, serde_json::to_string_pretty(session)?.as_bytes())
    }

    pub fn reset(&self) -> Result<()> {
        self.save(&PersistedSession { schema: SESSION_SCHEMA, shift: None, pending_end_shift: false })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{BatchInfo, SessionCounts};
    use crate::core::state::{PersistedShift, ShiftMeta};

    fn sample() -> PersistedSession {
        PersistedSession {
            schema: SESSION_SCHEMA,
            shift: Some(PersistedShift {
                meta: ShiftMeta { shift_id: "s1".into(), started_at_ms: 7, ended: false },
                batch: BatchInfo { id: "b1".into(), name: "B".into(), total: 10, completed: 3, failed: 1 },
                counts: SessionCounts { done: 3, failed: 1 },
            }),
            pending_end_shift: true,
        }
    }

    #[test]
    fn round_trip() {
        let dir = std::env::temp_dir().join(format!("station-test-{}", uuid::Uuid::new_v4()));
        let store = SessionStore::new(dir.join("session.json"));
        store.save(&sample()).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.shift.unwrap().counts, SessionCounts { done: 3, failed: 1 });
        assert!(loaded.pending_end_shift);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_file_is_fresh_state() {
        let store = SessionStore::new(std::env::temp_dir().join("station-none/never.json"));
        assert!(store.load().is_none());
    }

    #[test]
    fn corrupt_file_is_fresh_state_not_a_crash() {
        let dir = std::env::temp_dir().join(format!("station-test-{}", uuid::Uuid::new_v4()));
        let path = dir.join("session.json");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, b"{ not json").unwrap();
        assert!(SessionStore::new(path).load().is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn atomic_write_leaves_no_temp_behind() {
        let dir = std::env::temp_dir().join(format!("station-test-{}", uuid::Uuid::new_v4()));
        let path = dir.join("session.json");
        atomic_write(&path, b"one").unwrap();
        atomic_write(&path, b"two").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "two");
        assert!(!path.with_extension("tmp").exists());
        std::fs::remove_dir_all(&dir).ok();
    }
}
