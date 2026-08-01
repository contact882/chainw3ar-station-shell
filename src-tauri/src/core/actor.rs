//! The core actor: single owner of StationState. Every mutation flows through
//! `commit`, which enforces the recovery contract's ordering — persist to disk
//! FIRST, refresh the boot cache, THEN push to the page. Verdicts are emitted
//! the instant they exist; nothing here queues, paces, or coalesces them.

use crate::contract::{PushPayload, TapVerdict};
use crate::core::messages::{CoreMsg, EndShiftReply, SimStateView};
use crate::core::retry::{RetryLedger, ESCALATION_THRESHOLD};
use crate::core::state::{ShiftBeginKind, StationState};
use crate::config::OverrunPolicy;
use crate::persist::failures::{FailureLog, FailureRecord, OverrunLog, OverrunRecord};
use crate::persist::session::SessionStore;
use crate::push::Pusher;
use crate::seams::batch_source::{BatchSource, SimBatchSource};
use crate::seams::keying::{ChipPresentation, ForcedOutcome, KeyingOutcome, KeyingService, SimKeying};
use anyhow::anyhow;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tokio::time::Instant;

#[derive(Debug, Clone)]
pub struct Timing {
    /// End-shift is deliverable only this long after our last verdict — the
    /// UI drops the event mid-flash, and the longest flash (dead) is 2500ms.
    pub verdict_quiet_ms: u64,
    /// …and this long after the last boot pull (the UI drops events fired
    /// before its subscription attaches, ~100ms into a fresh document).
    pub boot_quiet_ms: u64,
    pub emit_spacing_ms: u64,
    pub tick_ms: u64,
}

impl Default for Timing {
    fn default() -> Self {
        Self { verdict_quiet_ms: 2600, boot_quiet_ms: 3000, emit_spacing_ms: 1000, tick_ms: 250 }
    }
}

/// How many spaced emissions under deliverable conditions before the shell's
/// bookkeeping declares the shift ended. One would suffice by construction;
/// the second is belt-and-braces (the UI ignores repeats on the summary).
const END_SHIFT_EMITS_TO_CONFIRM: u8 = 2;

pub struct CoreDeps {
    pub state: StationState,
    pub keying: Arc<dyn KeyingService>,
    pub batch_source: Arc<dyn BatchSource>,
    /// Sim handles for console/conformance control (same objects as above).
    pub sim_keying: Option<Arc<SimKeying>>,
    pub sim_batches: Option<Arc<SimBatchSource>>,
    pub session: SessionStore,
    pub failures: FailureLog,
    pub overruns: OverrunLog,
    pub overrun_policy: OverrunPolicy,
    pub boot_cache: Arc<crate::boot::BootCache>,
    pub pusher: Arc<dyn Pusher>,
    pub strings: Option<serde_json::Value>,
    pub timing: Timing,
    pub conformance: bool,
}

#[derive(Clone)]
pub struct CoreHandle {
    tx: mpsc::Sender<CoreMsg>,
}

impl CoreHandle {
    pub fn try_notify_boot_pulled(&self) {
        let _ = self.tx.try_send(CoreMsg::BootPulled);
    }

    pub async fn send(&self, msg: CoreMsg) -> anyhow::Result<()> {
        self.tx.send(msg).await.map_err(|_| anyhow!("core actor gone"))
    }

    /// For non-async threads (the PC/SC monitor).
    pub fn blocking_send(&self, msg: CoreMsg) {
        let _ = self.tx.blocking_send(msg);
    }

    async fn request<T>(
        &self,
        make: impl FnOnce(tokio::sync::oneshot::Sender<T>) -> CoreMsg,
    ) -> anyhow::Result<T> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.send(make(tx)).await?;
        rx.await.map_err(|_| anyhow!("core actor dropped reply"))
    }

    pub async fn list_batches(&self) -> anyhow::Result<Vec<crate::contract::BatchInfo>> {
        self.request(|reply| CoreMsg::ListBatches { reply }).await?
    }

    pub async fn select_batch(&self, id: String) -> anyhow::Result<PushPayload> {
        self.request(|reply| CoreMsg::SelectBatch { id, reply }).await?
    }

    pub async fn end_shift(&self) -> anyhow::Result<EndShiftReply> {
        self.request(|reply| CoreMsg::EndShiftCmd { reply }).await
    }

    pub async fn apply_batch(&self, id: String) -> anyhow::Result<()> {
        self.request(|reply| CoreMsg::ApplyBatch { id, reply }).await?
    }

    pub async fn swap_batch(&self, id: Option<String>) -> anyhow::Result<()> {
        self.request(|reply| CoreMsg::SwapBatch { id, reply }).await?
    }

    pub async fn conformance_reset(&self) -> anyhow::Result<()> {
        self.request(|reply| CoreMsg::ConformanceReset { reply }).await
    }

    pub async fn reset_demo(&self) -> anyhow::Result<()> {
        self.request(|reply| CoreMsg::ResetDemo { reply }).await
    }

    pub async fn sim_state(&self) -> anyhow::Result<SimStateView> {
        self.request(|reply| CoreMsg::GetSimState { reply }).await
    }
}

pub fn channel() -> (CoreHandle, mpsc::Receiver<CoreMsg>) {
    let (tx, rx) = mpsc::channel(256);
    (CoreHandle { tx }, rx)
}

pub struct CoreActor {
    state: StationState,
    retry: RetryLedger,
    deps: CoreDeps,
    last_verdict_at: Option<Instant>,
    last_boot_pull_at: Instant,
    last_end_shift_emit: Option<Instant>,
    end_shift_emits: u8,
    blip_until: Option<Instant>,
    stale_attaches: u64,
}

pub async fn run(mut rx: mpsc::Receiver<CoreMsg>, deps: CoreDeps) {
    let mut actor = CoreActor {
        state: StationState::new(deps.state.snapshot.config, None),
        retry: RetryLedger::default(),
        last_verdict_at: None,
        last_boot_pull_at: Instant::now(),
        last_end_shift_emit: None,
        end_shift_emits: 0,
        blip_until: None,
        stale_attaches: 0,
        deps,
    };
    // Take the pre-built state (moved via deps to keep construction in one place).
    actor.state = std::mem::replace(&mut actor.deps.state, StationState::new(actor.state.snapshot.config, None));
    // Seed the downstream half of the connection bit from the keying layer's
    // readiness probe; the reader monitor supplies the other half. (No flip
    // can happen yet — reader_present starts false — so nothing is pushed.)
    let ready = actor.deps.keying.ready();
    let _ = actor.state.set_connection_inputs(None, Some(ready));
    actor.refresh_boot_cache();

    let mut tick = tokio::time::interval(Duration::from_millis(actor.deps.timing.tick_ms));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            maybe = rx.recv() => match maybe {
                Some(msg) => actor.handle(msg).await,
                None => break,
            },
            _ = tick.tick() => actor.on_tick(),
        }
    }
}

impl CoreActor {
    fn now_ms() -> u64 {
        SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
    }

    fn refresh_boot_cache(&self) {
        self.deps
            .boot_cache
            .store(&self.state.boot_payload(self.deps.strings.clone()));
    }

    /// THE ordering invariant: persist → boot cache → push. Nothing observable
    /// leaves the process before the state that produced it is on disk. A
    /// persist FAILURE pauses the line (founder decision): the recovery
    /// contract can't be honored, so `connected` must not claim it can.
    fn commit(&mut self, payload: &PushPayload) {
        let saved = self.try_persist();
        self.refresh_boot_cache();
        self.deps.pusher.push(payload);
        self.update_persist_health(saved);
    }

    /// Persist without pushing (endShift bookkeeping, arm flags).
    fn persist_quietly(&mut self) {
        let saved = self.try_persist();
        self.refresh_boot_cache();
        self.update_persist_health(saved);
    }

    fn try_persist(&mut self) -> bool {
        match self.deps.session.save(&self.state.to_persisted()) {
            Ok(()) => true,
            Err(e) => {
                tracing::error!("session persist failed: {e}");
                false
            }
        }
    }

    /// Flip the line on persistence-health changes. The pause payload rides
    /// AFTER whatever verdict/data payload triggered the failed write, so
    /// the operator sees their tap's result, then LINE PAUSED.
    fn update_persist_health(&mut self, ok: bool) {
        if let Some(payload) = self.state.set_persist_ok(ok) {
            if ok {
                tracing::info!("session persistence recovered — line resuming");
                let _ = self.try_persist();
            } else {
                tracing::error!("session persistence BROKEN — pausing the line until it recovers");
            }
            self.refresh_boot_cache();
            self.deps.pusher.push(&payload);
        }
    }

    async fn handle(&mut self, msg: CoreMsg) {
        match msg {
            CoreMsg::ListBatches { reply } => {
                let _ = reply.send(self.deps.batch_source.list().await);
            }
            CoreMsg::SelectBatch { id, reply } => {
                let _ = reply.send(self.begin_shift(id, ShiftBeginKind::Select).await);
            }
            CoreMsg::ApplyBatch { id, reply } => {
                let _ = reply.send(self.begin_shift(id, ShiftBeginKind::Apply).await.map(|_| ()));
            }
            CoreMsg::SwapBatch { id, reply } => {
                let _ = reply.send(self.swap_batch(id).await);
            }
            CoreMsg::EndShiftCmd { reply } => {
                let counts = self.state.mark_shift_ended();
                self.persist_quietly();
                tracing::info!("shift ended by operator: done={} failed={}", counts.done, counts.failed);
                let _ = reply.send(EndShiftReply { counts, payload: None });
            }
            CoreMsg::BeginEndShift => {
                self.state.arm_end_shift();
                self.end_shift_emits = 0;
                self.last_end_shift_emit = None;
                self.persist_quietly();
                tracing::info!("shell-initiated end-shift armed");
            }
            CoreMsg::BridgeAttached { adapter_seq } => {
                tracing::info!("bridge attached (adapter seq {adapter_seq}, core seq {})", self.state.seq);
                if adapter_seq < self.state.seq {
                    // Stale boot (baked-fallback path): correct it, and count
                    // it — the reload spike asserts this never happens.
                    self.stale_attaches += 1;
                    tracing::warn!("adapter booted STALE (behind by {})", self.state.seq - adapter_seq);
                    let payload = self.state.catch_up();
                    self.commit(&payload);
                }
            }
            CoreMsg::BootPulled => {
                self.last_boot_pull_at = Instant::now();
            }
            CoreMsg::CardPresented { reader_id, atr, forced } => {
                self.on_card_presented(reader_id, atr, forced).await;
            }
            CoreMsg::ReaderPresence { present } => {
                if let Some(payload) = self.state.set_connection_inputs(Some(present), None) {
                    self.commit(&payload);
                }
            }
            CoreMsg::SetDownstream { ready } => {
                if let Some(sim) = &self.deps.sim_keying {
                    sim.arm(|p| p.ready = ready);
                }
                if let Some(payload) = self.state.set_connection_inputs(None, Some(ready)) {
                    self.commit(&payload);
                }
            }
            CoreMsg::Blip { ms } => {
                self.blip_until = Some(Instant::now() + Duration::from_millis(ms));
                if let Some(payload) = self.state.set_connection_inputs(None, Some(false)) {
                    self.commit(&payload);
                }
            }
            CoreMsg::GetSimState { reply } => {
                let policy = self.deps.sim_keying.as_ref().map(|k| k.policy());
                let _ = reply.send(SimStateView {
                    connection: if self.state.connected() { "connected" } else { "disconnected" },
                    reader_present: self.state.reader_present,
                    downstream_ready: self.state.downstream_ready,
                    batch_name: self.state.snapshot.current_batch.as_ref().map(|b| b.name.clone()),
                    counts: self.state.counts(),
                    pending_end_shift: self.state.pending_end_shift,
                    seq: self.state.seq,
                    sticky_chip: policy.as_ref().and_then(|p| p.sticky_chip.clone()),
                    forced_next: policy.as_ref().and_then(|p| p.forced_next.map(|f| format!("{f:?}").to_lowercase())),
                    latency_ms: policy.map(|p| p.latency_ms).unwrap_or(0),
                    stale_attaches: self.stale_attaches,
                });
            }
            CoreMsg::ConformanceReset { reply } => {
                let seq = self.state.seq;
                let config = self.state.snapshot.config;
                self.state = StationState::new(config, None);
                self.state.seq = seq;
                // Conformance runs "connected" with zero keying latency and no
                // spontaneous pushes — the payload here is deliberately dropped.
                let _ = self.state.set_connection_inputs(Some(true), Some(true));
                self.retry.clear_all();
                if let Some(b) = &self.deps.sim_batches {
                    b.reset();
                }
                if let Some(k) = &self.deps.sim_keying {
                    k.arm(|p| {
                        p.forced_next = None;
                        p.sticky_chip = None;
                        p.ready = true;
                        p.latency_ms = 0;
                    });
                }
                if let Err(e) = self.deps.session.reset() {
                    tracing::error!("session reset failed: {e}");
                }
                self.last_verdict_at = None;
                self.end_shift_emits = 0;
                self.refresh_boot_cache();
                let _ = reply.send(());
            }
            CoreMsg::ResetDemo { reply } => {
                let seq = self.state.seq;
                let config = self.state.snapshot.config;
                let (reader, downstream, persist) =
                    (self.state.reader_present, self.state.downstream_ready, self.state.persist_ok);
                self.state = StationState::new(config, None);
                self.state.seq = seq;
                let _ = self.state.set_connection_inputs(Some(reader), Some(downstream));
                let _ = self.state.set_persist_ok(persist);
                self.retry.clear_all();
                if let Some(b) = &self.deps.sim_batches {
                    b.reset();
                }
                if let Err(e) = self.deps.session.reset() {
                    tracing::error!("demo reset: session reset failed: {e}");
                }
                self.refresh_boot_cache();
                tracing::info!("demo data reset (sim console)");
                let _ = reply.send(());
            }
            CoreMsg::Tick => self.on_tick(),
        }
    }

    async fn begin_shift(&mut self, id: String, kind: ShiftBeginKind) -> anyhow::Result<PushPayload> {
        let batch = self
            .deps
            .batch_source
            .get(&id)
            .await?
            .ok_or_else(|| anyhow!("unknown batch: {id}"))?;
        let payload = self.state.begin_shift(
            batch,
            kind,
            uuid::Uuid::new_v4().to_string(),
            Self::now_ms(),
        );
        // A shift begin starts a fresh session — including the retry ledger.
        self.retry.clear_all();
        self.end_shift_emits = 0;
        self.commit(&payload);
        Ok(payload)
    }

    async fn swap_batch(&mut self, id: Option<String>) -> anyhow::Result<()> {
        let batch = match id {
            Some(id) => self
                .deps
                .batch_source
                .get(&id)
                .await?
                .ok_or_else(|| anyhow!("unknown batch: {id}"))?,
            None => {
                let current = self.state.snapshot.current_batch.as_ref().map(|b| b.id.clone());
                self.deps
                    .batch_source
                    .list()
                    .await?
                    .into_iter()
                    .find(|b| Some(&b.id) != current.as_ref())
                    .ok_or_else(|| anyhow!("no other batch to swap to"))?
            }
        };
        let payload = self.state.swap_batch(batch);
        self.commit(&payload);
        Ok(())
    }

    async fn on_card_presented(&mut self, reader_id: String, atr: Vec<u8>, forced: Option<ForcedOutcome>) {
        if !self.state.connected() {
            tracing::warn!("chip presented while disconnected — ignored (no verdict)");
            return;
        }
        let Some(batch) = self.state.snapshot.current_batch.clone() else {
            tracing::warn!("chip presented with no active batch — ignored");
            return;
        };
        // Overrun gate BEFORE keying: chips are consumed physically, so a
        // blocked tap must never reach the keying layer at all.
        if batch.completed >= batch.total && self.deps.overrun_policy == OverrunPolicy::Block {
            tracing::warn!(
                "tap past batch total blocked (policy=block, batch {} at {}/{})",
                batch.id, batch.completed, batch.total
            );
            return;
        }
        if let (Some(f), Some(sim)) = (forced, self.deps.sim_keying.as_ref()) {
            sim.arm(|p| p.forced_next = Some(f));
        }
        let presentation = ChipPresentation { reader_id: reader_id.clone(), at: SystemTime::now(), atr };
        let outcome = self.deps.keying.key_chip(&presentation, &batch.id).await;
        let shift_id = self.state.shift.as_ref().map(|s| s.shift_id.clone()).unwrap_or_default();

        let (verdict, chip_ref, class, attempt, escalated) = match outcome {
            KeyingOutcome::Keyed { chip_ref } => {
                self.retry.clear_chip(&chip_ref.0);
                (TapVerdict::Success, chip_ref, None, 0, false)
            }
            KeyingOutcome::Recoverable { class, chip_ref } => {
                let attempt = self.retry.record_failure(&chip_ref.0);
                let escalated = attempt >= ESCALATION_THRESHOLD;
                if escalated {
                    self.retry.clear_chip(&chip_ref.0);
                }
                let verdict = if escalated { TapVerdict::Dead } else { TapVerdict::Retry };
                (verdict, chip_ref, Some(class), attempt, escalated)
            }
            KeyingOutcome::Permanent { class, chip_ref } => {
                let attempt = self.retry.record_failure(&chip_ref.0);
                self.retry.clear_chip(&chip_ref.0);
                (TapVerdict::Dead, chip_ref, Some(class), attempt, false)
            }
            KeyingOutcome::NotReady => {
                tracing::warn!("keying layer not ready — no verdict; line pauses");
                if let Some(payload) = self.state.set_connection_inputs(None, Some(false)) {
                    self.commit(&payload);
                }
                return;
            }
        };

        if let Some(class) = &class {
            self.deps.failures.append(&FailureRecord {
                ts: Self::now_ms(),
                chip_ref: &chip_ref.0,
                class,
                verdict: match verdict {
                    TapVerdict::Retry => "retry",
                    _ => "dead",
                },
                attempt,
                escalated,
                reader_id: &reader_id,
                batch_id: &batch.id,
                shift_id: &shift_id,
            });
        }

        let updated_batch = if verdict != TapVerdict::Retry {
            match self.deps.batch_source.record_outcome(&batch.id, verdict).await {
                Ok(b) => Some(b),
                Err(e) => {
                    tracing::error!("record_outcome failed: {e} — counting session-side only");
                    None
                }
            }
        } else {
            None
        };

        // Overrun accounting (policy=allow): a success that lands past total
        // consumed a chip beyond the batch plan — record it distinctly so
        // "why 525/500?" has an exact, auditable answer.
        if verdict == TapVerdict::Success {
            if let Some(updated) = &updated_batch {
                if updated.completed > updated.total {
                    self.deps.overruns.append(&OverrunRecord {
                        ts: Self::now_ms(),
                        chip_ref: &chip_ref.0,
                        reader_id: &reader_id,
                        batch_id: &updated.id,
                        shift_id: &shift_id,
                        completed: updated.completed,
                        total: updated.total,
                        past_total: updated.completed - updated.total,
                    });
                    tracing::warn!(
                        "OVERRUN: batch {} now {}/{} (+{} past total)",
                        updated.id, updated.completed, updated.total,
                        updated.completed - updated.total
                    );
                }
            }
        }

        let payload = self.state.record_verdict(verdict, updated_batch);
        self.last_verdict_at = Some(Instant::now());
        self.commit(&payload);
    }

    fn on_tick(&mut self) {
        // Persistence-recovery probe: while broken, retry each tick; the line
        // resumes the moment a write succeeds.
        if !self.state.persist_ok {
            let ok = self.deps.session.save(&self.state.to_persisted()).is_ok();
            self.update_persist_health(ok);
        }

        if let Some(until) = self.blip_until {
            if Instant::now() >= until {
                self.blip_until = None;
                if let Some(sim) = &self.deps.sim_keying {
                    sim.arm(|p| p.ready = true);
                }
                if let Some(payload) = self.state.set_connection_inputs(None, Some(true)) {
                    self.commit(&payload);
                }
            }
        }

        if self.state.pending_end_shift && !self.deps.conformance {
            let timing = &self.deps.timing;
            let deliverable = self.state.connected()
                && self
                    .last_verdict_at
                    .map_or(true, |t| t.elapsed() >= Duration::from_millis(timing.verdict_quiet_ms))
                && self.last_boot_pull_at.elapsed() >= Duration::from_millis(timing.boot_quiet_ms)
                && self
                    .last_end_shift_emit
                    .map_or(true, |t| t.elapsed() >= Duration::from_millis(timing.emit_spacing_ms));
            if deliverable {
                let payload = self.state.emit_end_shift();
                self.commit(&payload);
                self.end_shift_emits += 1;
                self.last_end_shift_emit = Some(Instant::now());
                if self.end_shift_emits >= END_SHIFT_EMITS_TO_CONFIRM {
                    self.state.mark_shift_ended();
                    self.persist_quietly();
                    tracing::info!("shell-initiated end-shift confirmed delivered");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{FailBinSide, StationUiConfig, WireEvent};
    use crate::push::VecPusher;
    use crate::seams::keying::SimPolicy;

    struct Rig {
        handle: CoreHandle,
        pusher: Arc<VecPusher>,
        keying: Arc<SimKeying>,
        dir: std::path::PathBuf,
    }

    const RIG_FIXTURES: &str = r#"[
        {"id":"b1","name":"B ONE","total":10,"completed":0,"failed":0},
        {"id":"b2","name":"B TWO","total":5,"completed":0,"failed":0},
        {"id":"b-edge","name":"B EDGE","total":3,"completed":2,"failed":0}]"#;

    async fn rig(timing: Timing) -> Rig {
        rig_with(timing, OverrunPolicy::Allow, "session.json", |_| {}).await
    }

    async fn rig_with(
        timing: Timing,
        overrun_policy: OverrunPolicy,
        session_file: &str,
        prepare: impl FnOnce(&std::path::Path),
    ) -> Rig {
        let dir = std::env::temp_dir().join(format!("station-actor-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        prepare(&dir); // e.g. plant a persistence blocker BEFORE the actor's first commit
        let config = StationUiConfig { fail_bin_side: FailBinSide::Right };
        let keying = Arc::new(SimKeying::new(SimPolicy { latency_ms: 0, ..SimPolicy::default() }));
        let batches = Arc::new(SimBatchSource::from_fixture_json(RIG_FIXTURES, None).unwrap());
        let pusher = VecPusher::new();
        let state = StationState::new(config, None);
        let boot_cache = Arc::new(crate::boot::BootCache::new(&state.boot_payload(None)));
        let deps = CoreDeps {
            state,
            keying: keying.clone() as Arc<dyn KeyingService>,
            batch_source: batches.clone() as Arc<dyn BatchSource>,
            sim_keying: Some(keying.clone()),
            sim_batches: Some(batches),
            session: SessionStore::new(dir.join(session_file)),
            failures: FailureLog::new(dir.join("failures.jsonl")),
            overruns: OverrunLog::new(dir.join("overruns.jsonl")),
            overrun_policy,
            boot_cache,
            pusher: pusher.clone(),
            strings: None,
            timing,
            conformance: false,
        };
        let (handle, rx) = channel();
        tokio::spawn(run(rx, deps));
        // Bring the line up: reader present (downstream is seeded from the sim).
        handle.send(CoreMsg::ReaderPresence { present: true }).await.unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        Rig { handle, pusher, keying, dir }
    }

    fn verdicts(payloads: &[PushPayload]) -> Vec<TapVerdict> {
        payloads
            .iter()
            .flat_map(|p| &p.events)
            .filter_map(|e| match e {
                WireEvent::TapResult { result } => Some(*result),
                _ => None,
            })
            .collect()
    }

    async fn tap(rig: &Rig, forced: ForcedOutcome) {
        rig.handle
            .send(CoreMsg::CardPresented {
                reader_id: "test".into(),
                atr: vec![0x3b, 0x00],
                forced: Some(forced),
            })
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
    }

    #[tokio::test]
    async fn third_consecutive_recoverable_on_same_chip_escalates_to_dead() {
        let rig = rig(Timing::default()).await;
        rig.keying.arm(|p| p.sticky_chip = Some("chip-x".into()));
        rig.handle.select_batch("b1".into()).await.unwrap();
        rig.pusher.take();

        tap(&rig, ForcedOutcome::Recoverable).await;
        tap(&rig, ForcedOutcome::Recoverable).await;
        tap(&rig, ForcedOutcome::Recoverable).await;

        let payloads = rig.pusher.take();
        assert_eq!(
            verdicts(&payloads),
            vec![TapVerdict::Retry, TapVerdict::Retry, TapVerdict::Dead],
            "3rd consecutive recoverable failure on one chip must escalate"
        );
        let last = payloads.last().unwrap().snapshot.as_ref().unwrap();
        assert_eq!(last.session_counts.failed, 1, "only the escalation moves counts");
        assert_eq!(last.session_counts.done, 0);
        assert_eq!(last.current_batch.as_ref().unwrap().failed, 1);

        let records: Vec<serde_json::Value> = std::fs::read_to_string(rig.dir.join("failures.jsonl"))
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(records.len(), 3, "every failure gets a RECORD (the counter is never persisted)");
        assert_eq!(records[2]["escalated"], true);
        assert_eq!(records[2]["attempt"], 3);

        // A fresh chip after the escalation starts a fresh count.
        rig.keying.arm(|p| p.sticky_chip = Some("chip-y".into()));
        tap(&rig, ForcedOutcome::Recoverable).await;
        assert_eq!(verdicts(&rig.pusher.take()), vec![TapVerdict::Retry]);
    }

    #[tokio::test]
    async fn shift_begin_clears_the_retry_ledger() {
        let rig = rig(Timing::default()).await;
        rig.keying.arm(|p| p.sticky_chip = Some("chip-x".into()));
        rig.handle.select_batch("b1".into()).await.unwrap();
        tap(&rig, ForcedOutcome::Recoverable).await;
        tap(&rig, ForcedOutcome::Recoverable).await;
        // New shift — the 3rd failure must NOT escalate (ledger cleared).
        rig.handle.select_batch("b2".into()).await.unwrap();
        rig.pusher.take();
        tap(&rig, ForcedOutcome::Recoverable).await;
        assert_eq!(verdicts(&rig.pusher.take()), vec![TapVerdict::Retry]);
    }

    #[tokio::test]
    async fn success_pipeline_persists_before_it_pushes() {
        let rig = rig(Timing::default()).await;
        rig.handle.select_batch("b1".into()).await.unwrap();
        rig.pusher.take();
        tap(&rig, ForcedOutcome::Keyed).await;

        let payloads = rig.pusher.take();
        assert_eq!(payloads.len(), 1);
        assert_eq!(
            payloads[0].events,
            vec![WireEvent::TapResult { result: TapVerdict::Success }, WireEvent::Data]
        );
        // The session file already carries the count the push announced.
        let session: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(rig.dir.join("session.json")).unwrap()).unwrap();
        assert_eq!(session["shift"]["counts"]["done"], 1);
        assert_eq!(session["shift"]["batch"]["completed"], 1);
    }

    #[tokio::test]
    async fn shell_initiated_end_shift_re_emits_until_confirmed() {
        let timing = Timing { verdict_quiet_ms: 150, boot_quiet_ms: 0, emit_spacing_ms: 30, tick_ms: 10 };
        let rig = rig(timing).await;
        rig.handle.select_batch("b1".into()).await.unwrap();
        tap(&rig, ForcedOutcome::Keyed).await; // note: tap() itself settles ~30ms
        rig.pusher.take();

        rig.handle.send(CoreMsg::BeginEndShift).await.unwrap();
        // Still inside the verdict-quiet window: nothing may be emitted yet.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            rig.pusher.take().iter().all(|p| !p.events.contains(&WireEvent::EndShift)),
            "end-shift must not be emitted while a flash could still be on screen"
        );
        // After the quiet window: spaced re-emits until confirmed.
        tokio::time::sleep(Duration::from_millis(250)).await;
        let emits = rig
            .pusher
            .take()
            .iter()
            .filter(|p| p.events.contains(&WireEvent::EndShift))
            .count();
        assert_eq!(emits, 2, "exactly the confirm count, spaced, then stop");
        let session: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(rig.dir.join("session.json")).unwrap()).unwrap();
        assert_eq!(session["pendingEndShift"], false, "confirmed delivery is persisted");
    }

    /// S4/B4: overrun counting stays unclamped shell-side (+1 exactly per
    /// success) and every success past total writes a distinct OVERRUN
    /// record. Fails if clamping is introduced or the records are removed.
    #[tokio::test]
    async fn overrun_success_counts_unclamped_and_writes_records() {
        let rig = rig(Timing::default()).await;
        rig.handle.select_batch("b-edge".into()).await.unwrap(); // starts 2/3
        rig.pusher.take();
        for _ in 0..4 {
            tap(&rig, ForcedOutcome::Keyed).await; // → 3,4,5,6 of 3
        }
        let payloads = rig.pusher.take();
        let last = payloads.last().unwrap().snapshot.as_ref().unwrap();
        assert_eq!(last.current_batch.as_ref().unwrap().completed, 6, "true numbers, never clamped");
        assert_eq!(last.session_counts.done, 4, "+1 exactly per success");

        let records: Vec<serde_json::Value> = std::fs::read_to_string(rig.dir.join("overruns.jsonl"))
            .expect("overrun records must exist")
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(records.len(), 3, "successes landing PAST total (4,5,6 of 3) each record");
        assert_eq!(records[0]["pastTotal"], 1);
        assert_eq!(records[2]["pastTotal"], 3);
        assert_eq!(records[0]["batchId"], "b-edge");
        assert!(records[0]["chipRef"].as_str().unwrap().starts_with("sim-"));
    }

    /// S4/B4: under overrun_policy = block, a presentation at/past total
    /// produces NO keying call, no verdict, no chip consumption.
    #[tokio::test]
    async fn block_policy_refuses_taps_at_total_before_keying() {
        let rig = rig_with(Timing::default(), OverrunPolicy::Block, "session.json", |_| {}).await;
        rig.handle.select_batch("b-edge".into()).await.unwrap(); // 2/3
        rig.pusher.take();
        tap(&rig, ForcedOutcome::Keyed).await; // 2<3 → allowed → 3/3
        assert_eq!(verdicts(&rig.pusher.take()), vec![TapVerdict::Success]);
        tap(&rig, ForcedOutcome::Keyed).await; // at total → refused
        tap(&rig, ForcedOutcome::Keyed).await; // still refused
        let payloads = rig.pusher.take();
        assert!(
            verdicts(&payloads).is_empty(),
            "no verdict may be emitted for a blocked tap (and no chip consumed)"
        );
        assert!(
            !rig.dir.join("overruns.jsonl").exists(),
            "blocked taps never reach keying, so no overrun can be recorded"
        );
    }

    /// S8/C1: a persist failure pauses the line (connected must not claim a
    /// tap is fully processable when its count can't be saved), and the line
    /// resumes automatically once persistence recovers.
    #[tokio::test]
    async fn persist_failure_pauses_line_and_recovery_resumes_it() {
        let timing = Timing { tick_ms: 10, ..Timing::default() };
        // A FILE sits where the session's parent dir must go — planted BEFORE
        // the actor's first commit, so persistence is broken from the start.
        let rig = rig_with(timing, OverrunPolicy::Allow, "blocked/session.json", |dir| {
            std::fs::write(dir.join("blocked"), b"in the way").unwrap();
        })
        .await;
        let blocker = rig.dir.join("blocked");

        rig.handle.select_batch("b1".into()).await.unwrap(); // commit fails to persist
        tokio::time::sleep(Duration::from_millis(30)).await;
        let payloads = rig.pusher.take();
        assert!(
            payloads.iter().flat_map(|p| &p.events).any(|e| matches!(
                e,
                WireEvent::Connection { state: crate::contract::ConnectionState::Disconnected }
            )),
            "persist failure must pause the line"
        );
        // Taps while paused produce nothing (line is honest).
        tap(&rig, ForcedOutcome::Keyed).await;
        assert!(verdicts(&rig.pusher.take()).is_empty());

        // Persistence recovers → line resumes on its own within a few ticks.
        std::fs::remove_file(&blocker).unwrap();
        tokio::time::sleep(Duration::from_millis(120)).await;
        let payloads = rig.pusher.take();
        assert!(
            payloads.iter().flat_map(|p| &p.events).any(|e| matches!(
                e,
                WireEvent::Connection { state: crate::contract::ConnectionState::Connected }
            )),
            "line must resume when persistence recovers"
        );
        assert!(rig.dir.join("blocked/session.json").exists(), "state persisted after recovery");
    }

    #[tokio::test]
    async fn taps_while_disconnected_produce_no_verdict() {
        let rig = rig(Timing::default()).await;
        rig.handle.select_batch("b1".into()).await.unwrap();
        rig.handle.send(CoreMsg::ReaderPresence { present: false }).await.unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        rig.pusher.take();
        tap(&rig, ForcedOutcome::Keyed).await;
        assert!(rig.pusher.take().is_empty(), "a correct shell sends nothing while disconnected");
    }
}
