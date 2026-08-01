# SEAM.md — the exact interface the governed repository implements

This shell drives the frozen operator UI and owns verdict emission, per-chip
retry counting, session persistence, and batch state. **It contains no keying
logic, no APDUs, no sealed-blob handling, and no database path — by hard
boundary.** Two Rust traits below are the entire seam. This build ships
simulated implementations (`SimKeying`, `SimBatchSource`); the governed repo
replaces them and nothing else changes. This document is the buildable spec:
signatures, inputs, return shapes, error cases, threading, and the shell's
guarantees around every call.

Source of truth for the trait definitions: `src-tauri/src/seams/keying.rs`
and `src-tauri/src/seams/batch_source.rs`. This file annotates them; if the
two ever disagree, the .rs files win and this file has a bug.

---

## 1. `KeyingService` — the keying tool

```rust
pub struct ChipPresentation {
    pub reader_id: String,   // PC/SC reader name that saw the tap (log-grade id)
    pub at: SystemTime,      // presentation timestamp (shell clock)
    pub atr: Vec<u8>,        // passive ATR bytes from SCardGetStatusChange's
                             // reader state (card-TYPE info; empty for
                             // simulated taps). The shell never connects or
                             // transmits to obtain it. Internal-only data.
}

pub struct ChipRef(pub String);  // opaque per-chip identity token — see §1.3

pub enum KeyingOutcome {
    Keyed       { chip_ref: ChipRef },
    Recoverable { class: String, chip_ref: ChipRef },
    Permanent   { class: String, chip_ref: ChipRef },
    NotReady,
}

#[async_trait::async_trait]
pub trait KeyingService: Send + Sync {
    async fn key_chip(&self, presentation: &ChipPresentation, batch_id: &str) -> KeyingOutcome;
    fn ready(&self) -> bool;
}
```

### 1.1 `key_chip` — call contract

- **When called:** exactly once per chip presentation (EMPTY→PRESENT edge on
  the PC/SC reader, or a sim-console tap). Never called while the station is
  `disconnected` or without an active batch — the shell guards both.
- **Concurrency:** calls are strictly serialized. The shell's single state
  actor awaits each call to completion before processing anything else; there
  is never a second in-flight `key_chip`. No internal queueing is needed or
  wanted.
- **Timing:** the await duration is dead time between physical tap and
  operator feedback. There is no shell-imposed timeout — if keying can hang,
  the implementation must bound its own time and return `Recoverable` or
  `NotReady`. Design budget: p95 ≲ 1s; anything over ~2.5s starts to corrupt
  the tap→flash association the UI is built around.
- **Inputs:** `presentation.reader_id` is the PC/SC reader name (for your own
  logs/telemetry; the shell also writes it into failure records).
  `batch_id` is the opaque id of the active batch (from `BatchSource::list`).
  The implementation is expected to acquire the card connection itself (the
  shell holds NO card handle — its monitor never connects to the card, so
  the card is unclaimed when `key_chip` begins; use exclusive mode as needed).
- **Return, by variant — this is also the error model. The trait returns no
  `Result`; every failure is one of these variants:**
  - `Keyed { chip_ref }` — the chip is FULLY keyed and verified end-to-end
    (including whatever downstream write your layer owns). The shell emits a
    `success` verdict, increments session `done` and batch `completed`, and
    clears the chip's retry ladder.
  - `Recoverable { class, chip_ref }` — this attempt failed but a re-tap of
    the same chip may succeed (bad coupling, transient RF, retryable
    downstream error). The shell increments the chip's retry ladder: attempts
    1–2 emit `retry` (operator re-taps; nothing counted), attempt 3 emits
    `dead` (garment to the fail bin; counted as failed). **The shell never
    re-invokes `key_chip` itself — every attempt is a new physical tap.**
  - `Permanent { class, chip_ref }` — the chip is unusable (mis-keyed stock,
    broken chip, non-retryable state). Immediate `dead` verdict; counted.
  - `NotReady` — the layer cannot process ANY chip right now (keys not
    loaded, downstream unreachable). NOT a verdict: nothing is counted, no
    flash fires, and the shell flips the station to `disconnected` (the UI
    shows LINE PAUSED and inert-blocks the operator). Use this for every
    infrastructure-level failure; use `Recoverable` only for chip-scoped
    failures.
  - **Panics are a crash.** Do not panic across this boundary; map internal
    errors to `NotReady`.
- **`class`** is an opaque short code (≤ 16 chars suggested) chosen by the
  governed layer, written verbatim into `failures.jsonl` and shell logs for
  the future supervisor module. It is NEVER shown to operators — but treat it
  as confidentiality-bound anyway: no key names, no slot numbers, no APDU
  vocabulary.

### 1.2 `ready` — readiness probe

- Non-blocking, cheap, callable from any thread. `true` iff a `key_chip`
  call made right now would be processed end-to-end.
- The shell computes the UI's `connected` bit as
  `reader_present && downstream_ready`; `ready()` seeds `downstream_ready`
  at startup, and a `NotReady` outcome forces it false.
- **Push integration point (required for the real layer):** when readiness
  changes asynchronously (keys expire, downstream drops), send
  `CoreMsg::SetDownstream { ready }` to the core handle. The wiring site is
  `src-tauri/src/main.rs` (`setup`), where the reader monitor is spawned —
  spawn your readiness watcher the same way. Without this, a readiness loss
  is only discovered on the next tap (one wasted tap; still safe, but the
  operator deserves the earlier LINE PAUSED).

### 1.3 `chip_ref` — the identity requirement (load-bearing)

The shell's PC/SC monitor is detection-only (zero APDUs, founder-confirmed
boundary), so **the shell cannot see chip identity. Your result is its only
source.** Requirements:

- Opaque string; stable for the same physical chip at least within a shift
  (the retry ladder is keyed on it and is session-scoped by design).
- The sim's stand-in is `sha256(ATR ‖ presentation timestamp)`. **Known
  limitation, sim-mode only:** the ATR is card-TYPE data — every chip of the
  same model shares it — and the timestamp differs per tap, so sim identity
  is per-presentation. Sim-mode retry counting therefore cannot distinguish
  two chips of the same type, and cannot recognize the SAME chip re-tapped
  (each re-tap gets a fresh ref; the 3-strikes escalation is demonstrated via
  the sim console's SAME-GARMENT MODE, which pins a sticky ref). This is a
  deliberate consequence of the zero-APDU boundary, NOT a property of the
  design: real per-chip identity arrives with the governed layer's
  `chip_ref`, at which point escalation works per physical chip with no shell
  changes.
- Present on `Keyed`, `Recoverable`, AND `Permanent` — a Recoverable without
  a stable chip_ref breaks 3-strikes escalation silently.
- Never operator-visible; appears only in `failures.jsonl` and shell logs.
  Prefer a derived/HMAC'd token over a raw UID if raw UIDs are themselves
  sensitive — the shell does not care, only stability matters.

### 1.4 Complete error-case matrix (what the shell does with every outcome)

| Situation in the governed layer | You return | Shell behavior, exactly |
|---|---|---|
| Keyed + verified end-to-end | `Keyed{chip_ref}` | `success` verdict → UI green flash/ding; session `done`+1; `record_outcome(batch, success)` called; batch `completed`+1 rendered; retry ladder entry for `chip_ref` cleared |
| Chip-scoped, re-attemptable failure | `Recoverable{class, chip_ref}` | failure record written (attempt n); ladder n=1,2 → `retry` verdict (yellow flash, NOTHING counted, no data event); n=3 → `dead` verdict, `failed`+1 both levels, record `escalated:true`, ladder entry cleared |
| Chip unusable (permanent) | `Permanent{class, chip_ref}` | immediate `dead` verdict; `failed`+1 both levels; failure record |
| Layer-wide inability: keys unavailable, downstream down, caught internal error | `NotReady` | NO verdict, nothing counted, no flash; `downstream_ready=false` → station `disconnected` → UI holds LINE PAUSED until your readiness watcher reports recovery |
| `ready()` returned true but the tap then hits `NotReady` | (as above) | one wasted tap, line pauses — acceptable degraded path, logged |
| `key_chip` hangs | — | **forbidden.** The shell imposes no timeout by design (a late verdict against the wrong garment is worse than a paused line) and its watchdog watches the UI process, not this call — a hung `key_chip` is a hung station and it is your bug. Bound your own I/O and return `NotReady`/`Recoverable` |
| Panic across the boundary | — | **forbidden.** Process crash → external restart + crash-safe session recovery, but the operator eats a restart. Map internal errors to `NotReady` |
| `record_outcome` → `Err` | n/a | verdict still emitted (operator feedback never stalls on the write); session counts move; batch numbers go stale until your next successful return; error logged loudly; reconciliation is yours |
| `list`/`get` → `Err` | n/a | batch-select shows RETRY / `selectBatch` rejects; retried by operator action, not by the shell |
| `get(id)` → `Ok(None)` | n/a | `selectBatch` rejection (contract-required for unknown ids) |

Clock note: `presentation.at` is shell wall-clock (`SystemTime`) — it can jump
(NTP, manual set). Do not assume monotonicity; it exists for correlation and
logging, not ordering.

### 1.5 What the shell guarantees around your calls

- Persist-before-emit: session counts and batch progress are on disk before
  any verdict reaches the UI.
- The retry ladder (3rd consecutive `Recoverable` per chip_ref → `dead`) is
  the shell's, is cleared at shift begin and on `Keyed`, and is NEVER
  persisted across crashes (SHELL_INTEGRATION.md §4 REQUIRED — a failure
  RECORD is written per non-Keyed outcome instead: ts, chipRef, class,
  verdict, attempt, escalated, readerId, batchId, shiftId).
- Verdicts are emitted the instant your call returns. Nothing is queued,
  paced, or deduplicated — return an outcome only when it is final.

---

## 2. `BatchSource` — batch data + the guarded write

```rust
#[async_trait::async_trait]
pub trait BatchSource: Send + Sync {
    async fn list(&self) -> anyhow::Result<Vec<BatchInfo>>;
    async fn get(&self, id: &str) -> anyhow::Result<Option<BatchInfo>>;
    async fn record_outcome(&self, batch_id: &str, verdict: TapVerdict) -> anyhow::Result<BatchInfo>;
}

// BatchInfo (wire contract v1, frozen): { id, name, total, completed, failed }
```

- **`list`** — the selectable batches for this station. Fresh, defensively
  copied values each call (the UI may mutate what it receives). `name` is
  operator-visible: product wording only, confidentiality-bound. Errors
  (`Err`) are fine: the UI shows RETRY on batch select and calls again.
- **`get(id)`** — `Ok(None)` for an unknown id; the shell turns that into the
  `selectBatch` rejection the contract requires. `Err` = infrastructure
  failure (also rejects, logged).
- **`record_outcome(batch_id, verdict)`** — called once per counted verdict
  (`success` or `dead`; NEVER `retry`), after `key_chip` resolved and before
  the verdict is emitted. Returns the updated `BatchInfo` whose
  `completed`/`failed` already include this outcome — the shell renders your
  numbers verbatim and unclamped (401/400 is information, not an error).
  - Idempotency: the shell calls exactly once per verdict; it does not retry
    a failed `record_outcome`. On `Err`, the shell still counts the verdict
    session-side, emits it (operator feedback must not stall on the write),
    and logs the error loudly — reconciliation is the governed layer's
    concern. If your write is asynchronous/queued internally, return the
    optimistic batch numbers.
- Concurrency: serialized by the same single actor; no parallel calls.

---

## 3. Wiring checklist (mechanical, by design)

1. Add the governed crate as a dependency of `src-tauri`.
2. In `src-tauri/src/main.rs` (`fn main`), replace the `SimKeying::new(...)`
   and `SimBatchSource::from_fixture_json(...)` constructions with the
   governed implementations (both are consumed as `Arc<dyn KeyingService>` /
   `Arc<dyn BatchSource>`; the `sim_keying`/`sim_batches` fields of
   `CoreDeps` become `None`, which disables sim-only commands naturally).
3. Spawn the readiness watcher (§1.2) next to `reader::spawn` in `setup`.
4. `--sim` remains available for floor-demo mode ONLY if the sim
   implementations are kept behind a feature flag; otherwise delete the flag.
5. Re-run the gates: `cargo test` and `npm run conformance` must both pass
   unchanged — the conformance suite drives the real pipeline through
   `sim_tap_with_verdict`-style forced outcomes, so the harness needs the sim
   keying present; run the gate in a build with the sim feature enabled, or
   accept 3 skipped verdict tests against the governed build (gate policy
   decision for that repo).
6. The UI, contract files, adapter, persistence, retry doctrine, and reader
   monitor are untouched. If any of them needs a change to accommodate the
   governed layer, STOP — that is a contract conversation, not a wiring step.
