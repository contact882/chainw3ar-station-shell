//! Per-chip retry ledger — the "3rd failure = dead" escalation.
//!
//! SESSION-SCOPED BY DESIGN (SHELL_INTEGRATION.md §4, REQUIRED): this ledger
//! lives in memory, resets on crash recovery and at every shift begin, and is
//! NEVER persisted as authoritative state. Failure RECORDS go to the failures
//! log; the counter itself does not. Do not "fix" this.

use std::collections::HashMap;

pub const ESCALATION_THRESHOLD: u8 = 3;

#[derive(Debug, Default)]
pub struct RetryLedger {
    counts: HashMap<String, u8>,
}

impl RetryLedger {
    /// Records a recoverable failure for a chip and returns the attempt number
    /// (1-based). The caller escalates to `dead` at `ESCALATION_THRESHOLD`.
    pub fn record_failure(&mut self, chip_ref: &str) -> u8 {
        let n = self.counts.entry(chip_ref.to_string()).or_insert(0);
        *n = n.saturating_add(1);
        *n
    }

    /// A successful keying (or an escalation to dead) closes the chip's entry.
    pub fn clear_chip(&mut self, chip_ref: &str) {
        self.counts.remove(chip_ref);
    }

    /// Shift begin (selectBatch or apply-batch) starts a fresh session.
    pub fn clear_all(&mut self) {
        self.counts.clear();
    }

    #[cfg(test)]
    pub fn attempts(&self, chip_ref: &str) -> u8 {
        self.counts.get(chip_ref).copied().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn third_consecutive_failure_reaches_threshold() {
        let mut ledger = RetryLedger::default();
        assert_eq!(ledger.record_failure("chip-a"), 1);
        assert_eq!(ledger.record_failure("chip-a"), 2);
        assert_eq!(ledger.record_failure("chip-a"), 3);
        assert!(ledger.attempts("chip-a") >= ESCALATION_THRESHOLD);
    }

    #[test]
    fn chips_are_counted_independently() {
        let mut ledger = RetryLedger::default();
        ledger.record_failure("chip-a");
        ledger.record_failure("chip-a");
        assert_eq!(ledger.record_failure("chip-b"), 1);
        assert_eq!(ledger.attempts("chip-a"), 2);
    }

    #[test]
    fn success_clears_only_that_chip() {
        let mut ledger = RetryLedger::default();
        ledger.record_failure("chip-a");
        ledger.record_failure("chip-b");
        ledger.clear_chip("chip-a");
        assert_eq!(ledger.attempts("chip-a"), 0);
        assert_eq!(ledger.attempts("chip-b"), 1);
    }

    #[test]
    fn shift_begin_clears_everything() {
        let mut ledger = RetryLedger::default();
        ledger.record_failure("chip-a");
        ledger.record_failure("chip-b");
        ledger.clear_all();
        assert_eq!(ledger.attempts("chip-a"), 0);
        assert_eq!(ledger.attempts("chip-b"), 0);
    }
}
