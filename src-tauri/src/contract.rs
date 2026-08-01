//! Rust mirror of the frozen StationBridge wire contract (v1).
//!
//! The TypeScript source of truth is bridge/vendor/contract.ts (byte-pinned to
//! the UI's v0.2.0 tag; see PIN.json). These types serialize to exactly the
//! JSON shapes the injected adapter consumes. Field names are camelCase on the
//! wire; event tags are the contract's kebab-case literals.

use serde::{Deserialize, Serialize};

// Referenced by the pin-gate test and serialized checks; the init-script
// adapter carries its own copy from the vendored contract.ts.
#[allow(dead_code)]
pub const CONTRACT_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConnectionState {
    Connected,
    Disconnected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TapVerdict {
    Success,
    Retry,
    Dead,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchInfo {
    pub id: String,
    /// Operator-visible. Confidentiality-bound: outcome words / product names
    /// only — never failure reasons, identifiers, or technical vocabulary.
    pub name: String,
    pub total: u32,
    /// May exceed `total` (recounts). The UI renders unclamped by design.
    pub completed: u32,
    pub failed: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SessionCounts {
    pub done: u32,
    pub failed: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StationUiConfig {
    pub fail_bin_side: FailBinSide,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FailBinSide {
    Left,
    Right,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StationSnapshot {
    pub connection: ConnectionState,
    pub current_batch: Option<BatchInfo>,
    pub session_counts: SessionCounts,
    pub config: StationUiConfig,
}

/// Wire form of StationEvent. `data` / `apply-batch` carry NO snapshot on the
/// wire — the adapter substitutes its (just-updated) cache reference at
/// dispatch, which is what guarantees `event.snapshot === getSnapshot()`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum WireEvent {
    TapResult { result: TapVerdict },
    Connection { state: ConnectionState },
    Data,
    ApplyBatch,
    EndShift,
}

/// One atomic push to the adapter: snapshot swap first (when present), then
/// events dispatched in order. `seq` is monotonic for the process lifetime and
/// never resets — the adapter drops any payload with `seq <= lastSeq`, which
/// makes eval-channel and invoke-channel delivery race-free.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PushPayload {
    pub seq: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<StationSnapshot>,
    pub events: Vec<WireEvent>,
}

/// Response body of `GET station://boot` — pulled synchronously by the init
/// script on every document creation, so a reload hours into a shift still
/// boots from current truth.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BootPayload {
    pub seq: u64,
    pub snapshot: StationSnapshot,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strings: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_event_tags_match_contract_literals() {
        let cases = [
            (WireEvent::TapResult { result: TapVerdict::Success }, r#"{"type":"tap-result","result":"success"}"#),
            (WireEvent::TapResult { result: TapVerdict::Retry }, r#"{"type":"tap-result","result":"retry"}"#),
            (WireEvent::TapResult { result: TapVerdict::Dead }, r#"{"type":"tap-result","result":"dead"}"#),
            (WireEvent::Connection { state: ConnectionState::Disconnected }, r#"{"type":"connection","state":"disconnected"}"#),
            (WireEvent::Data, r#"{"type":"data"}"#),
            (WireEvent::ApplyBatch, r#"{"type":"apply-batch"}"#),
            (WireEvent::EndShift, r#"{"type":"end-shift"}"#),
        ];
        for (event, expected) in cases {
            assert_eq!(serde_json::to_string(&event).unwrap(), expected);
        }
    }

    /// Mirror of the build.rs pin gate (and scripts/check-contract-pin.mjs):
    /// `cargo test` alone must also refuse a drifted contract pairing.
    #[test]
    fn vendored_contract_files_match_the_frozen_pin() {
        use sha2::{Digest, Sha256};
        let vendor = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../bridge/vendor");
        let pin: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(vendor.join("PIN.json")).unwrap()).unwrap();
        assert_eq!(pin["contractVersion"], CONTRACT_VERSION, "wire CONTRACT_VERSION vs PIN");
        for (file, expected) in pin["sha256"].as_object().unwrap() {
            let raw = std::fs::read_to_string(vendor.join(file)).unwrap().replace("\r\n", "\n");
            let actual = Sha256::digest(raw.as_bytes())
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>();
            assert_eq!(&actual, expected.as_str().unwrap(), "{file} drifted from the frozen tag");
        }
    }

    #[test]
    fn snapshot_serializes_camel_case_with_null_batch() {
        let snapshot = StationSnapshot {
            connection: ConnectionState::Connected,
            current_batch: None,
            session_counts: SessionCounts::default(),
            config: StationUiConfig { fail_bin_side: FailBinSide::Right },
        };
        assert_eq!(
            serde_json::to_string(&snapshot).unwrap(),
            r#"{"connection":"connected","currentBatch":null,"sessionCounts":{"done":0,"failed":0},"config":{"failBinSide":"right"}}"#
        );
    }
}
