//! The core actor: single owner of StationState. Every mutation flows through
//! `commit`, which enforces the recovery contract's ordering — persist to disk
//! FIRST, refresh the boot cache, THEN push to the page. Verdicts are emitted
//! the instant they exist; nothing here queues, paces, or coalesces them.

use crate::contract::{PushPayload, TapVerdict};
use crate::core::messages::{CoreMsg, EndShiftReply, SimStateView};
use crate::core::retry::{RetryLedger, ESCALATION_THRESHOLD};
use crate::core::state::{ShiftBeginKind, StationState};
use crate::persist::failures::{FailureLog, FailureRecord};
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
    /// leaves the process before the state that produced it is on disk.
    fn commit(&mut self, payload: &PushPayload) {
        if let Err(e) = self.deps.session.save(&self.state.to_persisted()) {
            tracing::error!("session persist failed: {e}");
        }
        self.refresh_boot_cache();
        self.deps.pusher.push(payload);
    }

    /// Persist without pushing (endShift bookkeeping, arm flags).
    fn persist_quietly(&mut self) {
        if let Err(e) = self.deps.session.save(&self.state.to_persisted()) {
            tracing::error!("session persist failed: {e}");
        }
        self.refresh_boot_cache();
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

        let payload = self.state.record_verdict(verdict, updated_batch);
        self.last_verdict_at = Some(Instant::now());
        self.commit(&payload);
    }

    fn on_tick(&mut self) {
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

    async fn rig(timing: Timing) -> Rig {
        let dir = std::env::temp_dir().join(format!("station-actor-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let config = StationUiConfig { fail_bin_side: FailBinSide::Right };
        let keying = Arc::new(SimKeying::new(SimPolicy { latency_ms: 0, ..SimPolicy::default() }));
        let batches = Arc::new(
            SimBatchSource::from_fixture_json(
                r#"[{"id":"b1","name":"B ONE","total":10,"completed":0,"failed":0},
                    {"id":"b2","name":"B TWO","total":5,"completed":0,"failed":0}]"#,
                None,
            )
            .unwrap(),
        );
        let pusher = VecPusher::new();
        let state = StationState::new(config, None);
        let boot_cache = Arc::new(crate::boot::BootCache::new(&state.boot_payload(None)));
        let deps = CoreDeps {
            state,
            keying: keying.clone() as Arc<dyn KeyingService>,
            batch_source: batches.clone() as Arc<dyn BatchSource>,
            sim_keying: Some(keying.clone()),
            sim_batches: Some(batches),
            session: SessionStore::new(dir.join("session.json")),
            failures: FailureLog::new(dir.join("failures.jsonl")),
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
