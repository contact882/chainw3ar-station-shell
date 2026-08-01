// The entire boundary between this UI and the system that drives it.
// In this build a mock implements it; the Tauri shell later injects a real
// implementation as `window.__STATION__` before the bundle loads. No UI code
// outside station/index.ts may know which one it is talking to.
//
// The UI is a renderer: it never counts retries (the shell decides
// success/retry/dead per chip), never persists anything, and treats every
// snapshot as immutable.
//
// RECOVERY CONTRACT (the shell must honor these for crash safety):
// - The shell persists the active batch and sessionCounts across UI
//   reloads/crashes. A UI reload is lossless by design.
// - A non-null currentBatch at boot means "shift in progress" — the UI
//   resumes to READY and does NOT show batch select.
// - sessionCounts reset ONLY when a shift begins: operator `selectBatch`
//   or shell `apply-batch`. Both resets are executed by the shell.
// - An `end-shift` event delivered mid-flash is dropped by the UI; the
//   shell must retry it.
// - A shell-driven batch swap mid-shift (new currentBatch in a `data`
//   event) does NOT reset sessionCounts — counts are shift-scoped.
// - The per-chip retry counter is SESSION-SCOPED and resets on crash
//   recovery BY DESIGN (log failure records; never persist the counter
//   as authoritative state — see SHELL_INTEGRATION.md §4).

/**
 * Bumped on ANY breaking change to this file. The UI logs a fault on
 * mismatch at boot and continues best-effort; the shell's harness should
 * treat a mismatch as a build error. This contract is FROZEN as of v1 —
 * future changes are version bumps, never silent edits.
 */
export const CONTRACT_VERSION = 1;

export type ConnectionState = "connected" | "disconnected";

export type TapResult = "success" | "retry" | "dead";

// All snapshot data is readonly at the type level: consumers must never
// mutate what crosses this boundary — new state means a new object.
export interface BatchInfo {
  readonly id: string;
  readonly name: string;
  readonly total: number;
  readonly completed: number;
  readonly failed: number;
}

export interface SessionCounts {
  readonly done: number;
  readonly failed: number;
}

/** Static station-level configuration, set once by the shell at boot. */
export interface StationConfig {
  /**
   * Which side of the operator the failed bin physically sits — the DEAD
   * screen's directional cue mirrors to match opposite-handed stations.
   */
  readonly failBinSide: "left" | "right";
}

export interface StationSnapshot {
  readonly connection: ConnectionState;
  readonly currentBatch: BatchInfo | null;
  readonly sessionCounts: SessionCounts;
  readonly config: StationConfig;
}

export type StationEvent =
  | { type: "tap-result"; result: TapResult }
  | { type: "connection"; state: ConnectionState }
  | { type: "data"; snapshot: StationSnapshot }
  /**
   * Shell-initiated shift BEGIN: "here is the batch, begin." The snapshot
   * already reflects the assigned batch and fresh (zeroed) counts. Moves
   * the UI onto the operating screen from batch select or the summary.
   * For a mid-shift batch swap that KEEPS counts, emit `data` instead.
   */
  | { type: "apply-batch"; snapshot: StationSnapshot }
  /** Shell-initiated shift end (e.g. supervisor action on another screen). */
  | { type: "end-shift" };

export interface StationBridge {
  /** Must equal CONTRACT_VERSION of the contract.ts this UI was built against. */
  readonly contractVersion: number;
  /** Current snapshot. Must return a new object reference only when data changed. */
  getSnapshot(): StationSnapshot;
  /** Subscribe to events; returns an unsubscribe function. Events fire after the snapshot is updated. */
  subscribe(listener: (event: StationEvent) => void): () => void;
  listBatches(): Promise<BatchInfo[]>;
  /** Selecting a batch starts a fresh session: sessionCounts reset to zero. */
  selectBatch(id: string): Promise<void>;
  /**
   * Returns the final counts for the shift summary. Does not reset anything.
   * The UI renders the summary from its latest `data` snapshot — a real shell
   * that reconciles late results inside endShift() must also emit a final
   * `data` event so the summary reflects them.
   */
  endShift(): Promise<SessionCounts>;
}

declare global {
  interface Window {
    /** Injected by the Tauri shell before the bundle loads. */
    __STATION__?: StationBridge;
  }
}
