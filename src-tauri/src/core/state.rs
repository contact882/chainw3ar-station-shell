//! Pure station state + event composition. No IO here: the actor owns
//! persistence, seams, and pushing. Every mutation builds a NEW snapshot
//! (the old object is never touched after it leaves this struct) and bumps
//! the monotonic `seq` that stamps each push.

use crate::contract::{
    BatchInfo, ConnectionState, PushPayload, SessionCounts, StationSnapshot, StationUiConfig,
    TapVerdict, WireEvent,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShiftBeginKind {
    /// Operator picked a batch on the batch-select screen (`selectBatch`).
    Select,
    /// Shell-initiated begin (supervisor assignment) — emits `apply-batch`.
    Apply,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShiftMeta {
    pub shift_id: String,
    pub started_at_ms: u64,
    /// Set when the shift ended (operator endShift or delivered end-shift
    /// event). Deliberately does NOT touch the snapshot: `endShift` resets
    /// nothing, and a crash after end-of-shift resumes to READY with the old
    /// counts — the same behavior the UI's own mock has.
    pub ended: bool,
}

/// What `session.json` holds. The retry ledger is deliberately absent
/// (session-scoped by design — see core/retry.rs).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistedSession {
    pub schema: u32,
    pub shift: Option<PersistedShift>,
    pub pending_end_shift: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistedShift {
    #[serde(flatten)]
    pub meta: ShiftMeta,
    pub batch: BatchInfo,
    pub counts: SessionCounts,
}

pub const SESSION_SCHEMA: u32 = 1;

#[derive(Debug)]
pub struct StationState {
    pub snapshot: StationSnapshot,
    pub seq: u64,
    pub reader_present: bool,
    pub downstream_ready: bool,
    /// False while session persistence is failing (disk full, path broken).
    /// Folded into `connected`: a tap whose count would vanish on the next
    /// crash is NOT "fully processed", so the line pauses (founder decision).
    pub persist_ok: bool,
    pub shift: Option<ShiftMeta>,
    pub pending_end_shift: bool,
}

impl StationState {
    /// Boot state. Connection starts `disconnected` — truthful until the
    /// reader monitor and keying readiness report in (~1s), and fail-safe if
    /// they never do.
    pub fn new(config: StationUiConfig, persisted: Option<PersistedSession>) -> Self {
        let (shift, current_batch, counts, pending) = match persisted {
            Some(p) => match p.shift {
                Some(s) => (Some(s.meta), Some(s.batch), s.counts, p.pending_end_shift),
                None => (None, None, SessionCounts::default(), p.pending_end_shift),
            },
            None => (None, None, SessionCounts::default(), false),
        };
        Self {
            snapshot: StationSnapshot {
                connection: ConnectionState::Disconnected,
                current_batch,
                session_counts: counts,
                config,
            },
            seq: 0,
            reader_present: false,
            downstream_ready: false,
            persist_ok: true,
            shift,
            pending_end_shift: pending,
        }
    }

    pub fn connected(&self) -> bool {
        self.snapshot.connection == ConnectionState::Connected
    }

    pub fn counts(&self) -> SessionCounts {
        self.snapshot.session_counts
    }

    pub fn to_persisted(&self) -> PersistedSession {
        PersistedSession {
            schema: SESSION_SCHEMA,
            shift: match (&self.shift, &self.snapshot.current_batch) {
                (Some(meta), Some(batch)) => Some(PersistedShift {
                    meta: meta.clone(),
                    batch: batch.clone(),
                    counts: self.snapshot.session_counts,
                }),
                _ => None,
            },
            pending_end_shift: self.pending_end_shift,
        }
    }

    fn next_seq(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    fn payload(&mut self, snapshot_changed: bool, events: Vec<WireEvent>) -> PushPayload {
        let seq = self.next_seq();
        PushPayload {
            seq,
            snapshot: snapshot_changed.then(|| self.snapshot.clone()),
            events,
        }
    }

    /// Reader/downstream inputs. Returns a payload only on an actual
    /// connection flip; redundant reports are silent. Reconnect additionally
    /// carries the contract-mandated catch-up `data` event.
    pub fn set_connection_inputs(
        &mut self,
        reader_present: Option<bool>,
        downstream_ready: Option<bool>,
    ) -> Option<PushPayload> {
        if let Some(r) = reader_present {
            self.reader_present = r;
        }
        if let Some(d) = downstream_ready {
            self.downstream_ready = d;
        }
        self.recompute_connection()
    }

    /// Persistence health input — third leg of the `connected` bit.
    pub fn set_persist_ok(&mut self, ok: bool) -> Option<PushPayload> {
        self.persist_ok = ok;
        self.recompute_connection()
    }

    fn recompute_connection(&mut self) -> Option<PushPayload> {
        let next = if self.reader_present && self.downstream_ready && self.persist_ok {
            ConnectionState::Connected
        } else {
            ConnectionState::Disconnected
        };
        if next == self.snapshot.connection {
            return None;
        }
        self.snapshot = StationSnapshot { connection: next, ..self.snapshot.clone() };
        let events = match next {
            ConnectionState::Connected => vec![
                WireEvent::Connection { state: next },
                WireEvent::Data,
            ],
            ConnectionState::Disconnected => vec![WireEvent::Connection { state: next }],
        };
        Some(self.payload(true, events))
    }

    /// Shift begin — the ONLY place counts reset (selectBatch or apply-batch;
    /// both shell-executed). The retry ledger is cleared by the actor.
    pub fn begin_shift(&mut self, batch: BatchInfo, kind: ShiftBeginKind, shift_id: String, now_ms: u64) -> PushPayload {
        self.shift = Some(ShiftMeta { shift_id, started_at_ms: now_ms, ended: false });
        self.pending_end_shift = false;
        self.snapshot = StationSnapshot {
            current_batch: Some(batch),
            session_counts: SessionCounts::default(),
            ..self.snapshot.clone()
        };
        let event = match kind {
            ShiftBeginKind::Select => WireEvent::Data,
            ShiftBeginKind::Apply => WireEvent::ApplyBatch,
        };
        self.payload(true, vec![event])
    }

    /// Mid-shift batch swap: counts are shift-scoped and KEPT. The UI shows a
    /// 3s handoff pill on the batch-id change.
    pub fn swap_batch(&mut self, batch: BatchInfo) -> PushPayload {
        self.snapshot = StationSnapshot {
            current_batch: Some(batch),
            ..self.snapshot.clone()
        };
        self.payload(true, vec![WireEvent::Data])
    }

    /// A verdict, emitted the instant it exists. `retry` moves nothing: no
    /// snapshot change, no data event — exactly `[tap-result]`. Success/dead
    /// adopt the batch returned by the batch source and move the counts.
    pub fn record_verdict(&mut self, verdict: TapVerdict, updated_batch: Option<BatchInfo>) -> PushPayload {
        match verdict {
            TapVerdict::Retry => {
                self.payload(false, vec![WireEvent::TapResult { result: verdict }])
            }
            TapVerdict::Success | TapVerdict::Dead => {
                let counts = self.snapshot.session_counts;
                let session_counts = match verdict {
                    TapVerdict::Success => SessionCounts { done: counts.done + 1, ..counts },
                    _ => SessionCounts { failed: counts.failed + 1, ..counts },
                };
                self.snapshot = StationSnapshot {
                    current_batch: updated_batch.or_else(|| self.snapshot.current_batch.clone()),
                    session_counts,
                    ..self.snapshot.clone()
                };
                self.payload(
                    true,
                    vec![WireEvent::TapResult { result: verdict }, WireEvent::Data],
                )
            }
        }
    }

    /// Operator `endShift()`: resolve counts, reset NOTHING, push nothing.
    /// The `ended` flag is internal bookkeeping only.
    pub fn mark_shift_ended(&mut self) -> SessionCounts {
        if let Some(shift) = &mut self.shift {
            shift.ended = true;
        }
        self.pending_end_shift = false;
        self.snapshot.session_counts
    }

    /// Corrective push for an adapter that constructed itself from a stale
    /// baked snapshot (sync-XHR fallback path): current snapshot + one `data`.
    pub fn catch_up(&mut self) -> PushPayload {
        self.payload(true, vec![WireEvent::Data])
    }

    /// Shell-initiated shift end: arm the re-emit loop (persisted, so a shell
    /// crash mid-loop resumes re-emitting).
    pub fn arm_end_shift(&mut self) {
        self.pending_end_shift = true;
    }

    /// One end-shift emission attempt (the actor decides when delivery is
    /// actually possible and when to stop).
    pub fn emit_end_shift(&mut self) -> PushPayload {
        self.payload(false, vec![WireEvent::EndShift])
    }

    pub fn boot_payload(&self, strings: Option<serde_json::Value>) -> crate::contract::BootPayload {
        crate::contract::BootPayload {
            seq: self.seq,
            snapshot: self.snapshot.clone(),
            strings,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::FailBinSide;

    fn cfg() -> StationUiConfig {
        StationUiConfig { fail_bin_side: FailBinSide::Right }
    }

    fn batch(id: &str, completed: u32, failed: u32) -> BatchInfo {
        BatchInfo { id: id.into(), name: id.to_uppercase(), total: 100, completed, failed }
    }

    fn fresh_connected() -> StationState {
        let mut s = StationState::new(cfg(), None);
        s.set_connection_inputs(Some(true), Some(true)).unwrap();
        s
    }

    #[test]
    fn boot_is_disconnected_until_inputs_arrive() {
        let s = StationState::new(cfg(), None);
        assert_eq!(s.snapshot.connection, ConnectionState::Disconnected);
    }

    #[test]
    fn crash_recovery_restores_batch_and_counts() {
        let persisted = PersistedSession {
            schema: SESSION_SCHEMA,
            shift: Some(PersistedShift {
                meta: ShiftMeta { shift_id: "s1".into(), started_at_ms: 1, ended: false },
                batch: batch("b1", 12, 1),
                counts: SessionCounts { done: 12, failed: 1 },
            }),
            pending_end_shift: false,
        };
        let s = StationState::new(cfg(), Some(persisted));
        assert_eq!(s.snapshot.current_batch.as_ref().unwrap().id, "b1");
        assert_eq!(s.snapshot.session_counts, SessionCounts { done: 12, failed: 1 });
    }

    #[test]
    fn begin_shift_resets_counts_and_picks_the_event_kind() {
        let mut s = fresh_connected();
        s.record_verdict(TapVerdict::Success, Some(batch("b1", 1, 0)));
        let p = s.begin_shift(batch("b2", 0, 0), ShiftBeginKind::Select, "s2".into(), 0);
        assert_eq!(s.snapshot.session_counts, SessionCounts::default());
        assert_eq!(p.events, vec![WireEvent::Data]);
        assert!(p.snapshot.is_some());

        let p = s.begin_shift(batch("b3", 0, 0), ShiftBeginKind::Apply, "s3".into(), 0);
        assert_eq!(p.events, vec![WireEvent::ApplyBatch]);
        assert_eq!(
            p.snapshot.unwrap().current_batch.unwrap().id,
            "b3",
            "the apply-batch payload snapshot must already carry the batch"
        );
    }

    #[test]
    fn swap_keeps_counts() {
        let mut s = fresh_connected();
        s.begin_shift(batch("b1", 0, 0), ShiftBeginKind::Select, "s1".into(), 0);
        s.record_verdict(TapVerdict::Success, Some(batch("b1", 1, 0)));
        let p = s.swap_batch(batch("b2", 50, 2));
        assert_eq!(s.snapshot.session_counts.done, 1, "counts are shift-scoped, not batch-scoped");
        assert_eq!(p.events, vec![WireEvent::Data]);
    }

    #[test]
    fn success_verdict_moves_done_and_orders_events() {
        let mut s = fresh_connected();
        s.begin_shift(batch("b1", 10, 0), ShiftBeginKind::Select, "s1".into(), 0);
        let p = s.record_verdict(TapVerdict::Success, Some(batch("b1", 11, 0)));
        assert_eq!(s.snapshot.session_counts, SessionCounts { done: 1, failed: 0 });
        assert_eq!(s.snapshot.current_batch.as_ref().unwrap().completed, 11);
        assert_eq!(
            p.events,
            vec![WireEvent::TapResult { result: TapVerdict::Success }, WireEvent::Data],
            "tap-result must precede data"
        );
        assert!(p.snapshot.is_some());
    }

    #[test]
    fn dead_verdict_moves_failed() {
        let mut s = fresh_connected();
        s.begin_shift(batch("b1", 10, 1), ShiftBeginKind::Select, "s1".into(), 0);
        let p = s.record_verdict(TapVerdict::Dead, Some(batch("b1", 10, 2)));
        assert_eq!(s.snapshot.session_counts, SessionCounts { done: 0, failed: 1 });
        assert_eq!(s.snapshot.current_batch.as_ref().unwrap().failed, 2);
        assert!(p.snapshot.is_some());
    }

    #[test]
    fn retry_verdict_moves_nothing_and_emits_no_data() {
        let mut s = fresh_connected();
        s.begin_shift(batch("b1", 10, 0), ShiftBeginKind::Select, "s1".into(), 0);
        let before = s.snapshot.clone();
        let p = s.record_verdict(TapVerdict::Retry, None);
        assert_eq!(s.snapshot, before, "retry is not a verdict — nothing moves");
        assert_eq!(p.events, vec![WireEvent::TapResult { result: TapVerdict::Retry }]);
        assert!(p.snapshot.is_none(), "no snapshot on the wire when nothing changed");
    }

    #[test]
    fn reconnect_carries_catch_up_data_but_disconnect_does_not() {
        let mut s = fresh_connected();
        let p = s.set_connection_inputs(Some(false), None).unwrap();
        assert_eq!(p.events, vec![WireEvent::Connection { state: ConnectionState::Disconnected }]);
        let p = s.set_connection_inputs(Some(true), None).unwrap();
        assert_eq!(
            p.events,
            vec![
                WireEvent::Connection { state: ConnectionState::Connected },
                WireEvent::Data,
            ]
        );
        assert!(s.set_connection_inputs(Some(true), None).is_none(), "redundant input is silent");
    }

    #[test]
    fn end_shift_resets_nothing() {
        let mut s = fresh_connected();
        s.begin_shift(batch("b1", 0, 0), ShiftBeginKind::Select, "s1".into(), 0);
        s.record_verdict(TapVerdict::Success, Some(batch("b1", 1, 0)));
        let seq_before = s.seq;
        let counts = s.mark_shift_ended();
        assert_eq!(counts, SessionCounts { done: 1, failed: 0 });
        assert_eq!(s.snapshot.session_counts, counts, "endShift must not reset the session");
        assert!(s.snapshot.current_batch.is_some(), "snapshot untouched until next shift begin");
        assert_eq!(s.seq, seq_before, "endShift pushes nothing");
    }

    #[test]
    fn seq_is_strictly_monotonic() {
        let mut s = fresh_connected();
        let a = s.begin_shift(batch("b1", 0, 0), ShiftBeginKind::Select, "s1".into(), 0).seq;
        let b = s.record_verdict(TapVerdict::Retry, None).seq;
        let c = s.emit_end_shift().seq;
        assert!(a < b && b < c);
    }

    #[test]
    fn persistence_round_trip() {
        let mut s = fresh_connected();
        s.begin_shift(batch("b1", 5, 1), ShiftBeginKind::Select, "s1".into(), 42);
        s.record_verdict(TapVerdict::Success, Some(batch("b1", 6, 1)));
        s.arm_end_shift();
        let p = s.to_persisted();
        let restored = StationState::new(cfg(), Some(serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap()));
        assert_eq!(restored.snapshot.current_batch, s.snapshot.current_batch);
        assert_eq!(restored.snapshot.session_counts, s.snapshot.session_counts);
        assert!(restored.pending_end_shift);
    }
}
