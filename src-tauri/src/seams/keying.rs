//! THE KEYING SEAM.
//!
//! The real keying tool lives in a separate governed repository and is NOT on
//! this station. This trait is the exact surface the shell calls; SEAM.md is
//! the buildable specification the governed repo implements against. This
//! repo contains ONLY the simulated implementation — no keying logic, no APDU
//! sequences, no slot constants, no sealed-blob handling, ever.

use crate::contract::TapVerdict;
use std::time::{Duration, SystemTime};

/// A chip presentation as observed by the (detection-only) reader monitor.
/// Contains NO chip identity — identity is returned by the keying layer.
/// `atr` is the passive answer-to-reset bytes SCardGetStatusChange reports
/// with card presence (card-TYPE information, not per-chip identity; empty
/// for simulated presentations). Log-internal only.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ChipPresentation {
    pub reader_id: String,
    pub at: SystemTime,
    pub atr: Vec<u8>,
}

/// Opaque per-chip identity token, minted by the keying layer. The shell uses
/// it ONLY to correlate retries within a session and in failure records; it is
/// never operator-visible.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChipRef(pub String);

/// Outcome of one keying attempt.
#[derive(Debug, Clone)]
pub enum KeyingOutcome {
    /// Chip fully keyed and verified end-to-end.
    Keyed { chip_ref: ChipRef },
    /// Attempt failed but the chip is re-attemptable. `class` is an opaque
    /// short code for diagnostics (log-only, never shown to operators).
    Recoverable { class: String, chip_ref: ChipRef },
    /// Chip is unusable (mis-keyed, damaged, wrong stock). Verdict: dead.
    Permanent { class: String, chip_ref: ChipRef },
    /// The keying layer cannot process ANY chip right now. Not a verdict —
    /// flips the station to disconnected (the `connected` bit means a tap
    /// will be processed end-to-end).
    NotReady,
}

#[async_trait::async_trait]
pub trait KeyingService: Send + Sync {
    async fn key_chip(&self, presentation: &ChipPresentation, batch_id: &str) -> KeyingOutcome;
    /// Cheap, non-blocking readiness probe; feeds the `connected` bit.
    fn ready(&self) -> bool;
}

/// What the sim console can arm for the next presentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ForcedOutcome {
    Keyed,
    Recoverable,
    Permanent,
    Notready,
}

#[derive(Debug, Clone)]
pub struct SimPolicy {
    /// Consumed by the next presentation, then cleared.
    pub forced_next: Option<ForcedOutcome>,
    /// Weighted default for unforced (real-reader) taps.
    pub weight_keyed: u32,
    pub weight_recoverable: u32,
    pub weight_permanent: u32,
    pub latency_ms: u64,
    /// When set, every simulated presentation reports this chip_ref — lets the
    /// sim console demonstrate the 3-consecutive-failures escalation.
    pub sticky_chip: Option<String>,
    pub ready: bool,
}

impl Default for SimPolicy {
    fn default() -> Self {
        Self {
            forced_next: None,
            weight_keyed: 85,
            weight_recoverable: 12,
            weight_permanent: 3,
            latency_ms: 250,
            sticky_chip: None,
            ready: true,
        }
    }
}

/// Simulated keying: draws outcomes from the armed policy. The chip_ref is
/// fabricated (a fresh UUID per presentation unless sticky) because the
/// zero-APDU boundary means the shell never learns real chip identity —
/// that arrives with the governed layer.
pub struct SimKeying {
    policy: std::sync::Mutex<SimPolicy>,
}

impl SimKeying {
    pub fn new(policy: SimPolicy) -> Self {
        Self { policy: std::sync::Mutex::new(policy) }
    }

    pub fn arm(&self, update: impl FnOnce(&mut SimPolicy)) {
        update(&mut self.policy.lock().expect("sim policy lock"));
    }

    pub fn policy(&self) -> SimPolicy {
        self.policy.lock().expect("sim policy lock").clone()
    }

    fn draw(&self) -> (ForcedOutcome, u64, Option<String>) {
        let mut policy = self.policy.lock().expect("sim policy lock");
        let outcome = match policy.forced_next.take() {
            Some(forced) => forced,
            None => {
                if !policy.ready {
                    ForcedOutcome::Notready
                } else {
                    let total = policy.weight_keyed + policy.weight_recoverable + policy.weight_permanent;
                    let roll = if total == 0 { 0 } else { rand::random_range(0..total) };
                    if roll < policy.weight_keyed {
                        ForcedOutcome::Keyed
                    } else if roll < policy.weight_keyed + policy.weight_recoverable {
                        ForcedOutcome::Recoverable
                    } else {
                        ForcedOutcome::Permanent
                    }
                }
            }
        };
        (outcome, policy.latency_ms, policy.sticky_chip.clone())
    }
}

/// Fabricated chip identity: sha256(ATR bytes ‖ presentation timestamp),
/// truncated. NOTHING chip-unique goes in (the ATR is card-type data that
/// arrives passively with the status change; the UID is never read) — real
/// per-chip identity arrives only with the governed keying layer's results.
/// Timestamp-only when no card was involved (sim-console taps).
///
/// LIMITATION (sim-mode only, by design): same-model chips share an ATR and
/// the timestamp is per-tap, so these refs are per-PRESENTATION — sim retry
/// counting cannot distinguish two chips of the same type nor recognize a
/// re-tap of the same chip. Escalation demos use the sim console's sticky
/// chip. Real per-chip escalation begins the day the governed layer's
/// chip_ref replaces this (SEAM.md §1.3); no shell changes needed.
pub fn fabricate_chip_ref(atr: &[u8], at: SystemTime) -> String {
    use sha2::{Digest, Sha256};
    let nanos = at
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut hasher = Sha256::new();
    hasher.update(atr);
    hasher.update(nanos.to_le_bytes());
    let digest = hasher.finalize();
    let hex: String = digest.iter().take(12).map(|b| format!("{b:02x}")).collect();
    format!("sim-{hex}")
}

#[async_trait::async_trait]
impl KeyingService for SimKeying {
    async fn key_chip(&self, presentation: &ChipPresentation, _batch_id: &str) -> KeyingOutcome {
        let (outcome, latency_ms, sticky) = self.draw();
        if latency_ms > 0 {
            tokio::time::sleep(Duration::from_millis(latency_ms)).await;
        }
        let chip_ref = ChipRef(
            sticky.unwrap_or_else(|| fabricate_chip_ref(&presentation.atr, presentation.at)),
        );
        match outcome {
            ForcedOutcome::Keyed => KeyingOutcome::Keyed { chip_ref },
            ForcedOutcome::Recoverable => KeyingOutcome::Recoverable { class: "S1".into(), chip_ref },
            ForcedOutcome::Permanent => KeyingOutcome::Permanent { class: "S2".into(), chip_ref },
            ForcedOutcome::Notready => KeyingOutcome::NotReady,
        }
    }

    fn ready(&self) -> bool {
        self.policy.lock().expect("sim policy lock").ready
    }
}

/// Map a requested conformance verdict onto the sim policy so the REAL
/// pipeline produces exactly that verdict: fresh chip_ref every call, so a
/// single Recoverable can never trip the 3-strikes escalation.
pub fn force_for_verdict(verdict: TapVerdict) -> ForcedOutcome {
    match verdict {
        TapVerdict::Success => ForcedOutcome::Keyed,
        TapVerdict::Retry => ForcedOutcome::Recoverable,
        TapVerdict::Dead => ForcedOutcome::Permanent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fabricated_refs_are_unique_per_presentation_and_carry_nothing_raw() {
        let atr = [0x3b, 0x8f, 0x80, 0x01, 0x80, 0x4f];
        // Windows SystemTime has 100ns tick granularity — use values that
        // survive the round-trip distinctly.
        let t1 = SystemTime::UNIX_EPOCH + Duration::from_micros(1);
        let t2 = SystemTime::UNIX_EPOCH + Duration::from_micros(2);
        let a = fabricate_chip_ref(&atr, t1);
        let b = fabricate_chip_ref(&atr, t2);
        assert_ne!(a, b, "same card type, different presentations → different refs");
        assert_eq!(a, fabricate_chip_ref(&atr, t1), "deterministic");
        assert!(a.starts_with("sim-") && a.len() == 4 + 24);
        // Nothing of the ATR appears verbatim in the token.
        assert!(!a.contains("3b8f"));
        // Timestamp-only path (no card, e.g. sim-console taps) works too.
        assert_ne!(fabricate_chip_ref(&[], t1), fabricate_chip_ref(&[], t2));
    }
}
