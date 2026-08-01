//! Messages into the core actor — the single owner of station truth.

use crate::contract::{BatchInfo, PushPayload, SessionCounts};
use crate::seams::keying::ForcedOutcome;
use serde::Serialize;
use tokio::sync::oneshot;

pub type Reply<T> = oneshot::Sender<T>;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EndShiftReply {
    pub counts: SessionCounts,
    /// Present only if late reconciliation changed data (none in the sim).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<PushPayload>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SimStateView {
    pub connection: &'static str,
    pub reader_present: bool,
    pub downstream_ready: bool,
    pub batch_name: Option<String>,
    pub counts: SessionCounts,
    pub pending_end_shift: bool,
    pub seq: u64,
    pub sticky_chip: Option<String>,
    pub forced_next: Option<String>,
    pub latency_ms: u64,
    /// How many bridge_attached announcements arrived with a seq BEHIND core
    /// truth — i.e. documents that booted from a stale (baked) snapshot
    /// instead of the sync pull. The reload spike asserts this stays 0.
    pub stale_attaches: u64,
}

pub enum CoreMsg {
    // ── UI adapter commands ────────────────────────────────────────────────
    ListBatches { reply: Reply<anyhow::Result<Vec<BatchInfo>>> },
    SelectBatch { id: String, reply: Reply<anyhow::Result<PushPayload>> },
    EndShiftCmd { reply: Reply<EndShiftReply> },
    /// Adapter announces a fresh document with the seq it booted from; the
    /// actor replies with a corrective push iff truth has moved past it.
    BridgeAttached { adapter_seq: u64 },
    BootPulled,

    // ── Hardware / sim tap path (both funnel here) ─────────────────────────
    /// `atr` is captured PASSIVELY from SCardGetStatusChange's reader state —
    /// no card connection, no transmit. Empty for simulated taps (no card).
    /// Internal use only: it seasons the fabricated chip ref hash and must
    /// never reach an operator surface.
    CardPresented { reader_id: String, atr: Vec<u8>, forced: Option<ForcedOutcome> },
    ReaderPresence { present: bool },

    // ── Sim console / supervisor ───────────────────────────────────────────
    ApplyBatch { id: String, reply: Reply<anyhow::Result<()>> },
    SwapBatch { id: Option<String>, reply: Reply<anyhow::Result<()>> },
    BeginEndShift,
    SetDownstream { ready: bool },
    Blip { ms: u64 },
    GetSimState { reply: Reply<SimStateView> },

    /// Sim-console demo reset: wipe accumulated sim batch progress + session,
    /// keep connection inputs. The command reloads the UI afterward so it
    /// boots fresh from the reset snapshot.
    ResetDemo { reply: Reply<()> },

    // ── Plumbing ───────────────────────────────────────────────────────────
    /// External tick injection (tests); the actor also self-ticks.
    #[allow(dead_code)]
    Tick,
    ConformanceReset { reply: Reply<()> },
}
