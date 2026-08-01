// A fake of the WIRE PROTOCOL only — not of Rust's logic. It implements the
// same command surface + push composition rules the core actor guarantees, so
// the vendored conformance suite can exercise the real adapter in vitest.
// Everything Rust actually owns (retry escalation, persistence, hardware) is
// covered by cargo test; the in-webview gate remains the only merge gate.

import type { AdapterIo, BootPayload, WireEvent, WirePush } from "../src/adapter";
import type { BatchInfo, SessionCounts, StationSnapshot, TapResult } from "../vendor/contract";

interface FakeState {
  seq: number;
  connection: "connected" | "disconnected";
  batch: BatchInfo | null;
  counts: SessionCounts;
}

const FIXTURES: readonly BatchInfo[] = [
  { id: "b1", name: "B ONE", total: 10, completed: 2, failed: 1 },
  { id: "b2", name: "B TWO", total: 5, completed: 0, failed: 0 },
];

export class FakeBackend {
  private state: FakeState = { seq: 0, connection: "connected", batch: null, counts: { done: 0, failed: 0 } };
  private progress = new Map<string, { completed: number; failed: number }>();
  private push: (p: WirePush) => void = () => {};

  /** The harness rebinds this to the freshly installed adapter's applyPush. */
  bindPush(push: (p: WirePush) => void): void {
    this.push = push;
  }

  reset(): void {
    this.state = { seq: this.state.seq, connection: "connected", batch: null, counts: { done: 0, failed: 0 } };
    this.progress.clear();
  }

  private snapshot(): StationSnapshot {
    return {
      connection: this.state.connection,
      currentBatch: this.state.batch ? { ...this.state.batch } : null,
      sessionCounts: { ...this.state.counts },
      config: { failBinSide: "right" },
    };
  }

  boot(): BootPayload {
    return { seq: this.state.seq, snapshot: this.snapshot() };
  }

  private merged(id: string): BatchInfo | undefined {
    const fixture = FIXTURES.find((b) => b.id === id);
    if (!fixture) return undefined;
    const p = this.progress.get(id) ?? { completed: 0, failed: 0 };
    return { ...fixture, completed: fixture.completed + p.completed, failed: fixture.failed + p.failed };
  }

  private compose(snapshotChanged: boolean, events: WireEvent[]): WirePush {
    this.state.seq += 1;
    const push: WirePush = { seq: this.state.seq, events };
    if (snapshotChanged) push.snapshot = this.snapshot();
    return push;
  }

  fireTap(result: TapResult): void {
    if (this.state.connection !== "connected" || !this.state.batch) return;
    if (result === "retry") {
      this.push(this.compose(false, [{ type: "tap-result", result }]));
      return;
    }
    const p = this.progress.get(this.state.batch.id) ?? { completed: 0, failed: 0 };
    if (result === "success") {
      p.completed += 1;
      this.state.counts = { done: this.state.counts.done + 1, failed: this.state.counts.failed };
    } else {
      p.failed += 1;
      this.state.counts = { done: this.state.counts.done, failed: this.state.counts.failed + 1 };
    }
    this.progress.set(this.state.batch.id, p);
    this.state.batch = this.merged(this.state.batch.id) ?? this.state.batch;
    this.push(this.compose(true, [{ type: "tap-result", result }, { type: "data" }]));
  }

  applyBatch(id: string): void {
    const batch = this.merged(id);
    if (!batch) throw new Error(`unknown batch: ${id}`);
    this.state.batch = batch;
    this.state.counts = { done: 0, failed: 0 };
    this.push(this.compose(true, [{ type: "apply-batch" }]));
  }

  io(): AdapterIo {
    return {
      invoke: async <T,>(cmd: string, args?: Record<string, unknown>): Promise<T> => {
        switch (cmd) {
          case "list_batches":
            return FIXTURES.map((b) => this.merged(b.id)!) as unknown as T;
          case "select_batch": {
            const id = args?.id as string;
            const batch = this.merged(id);
            if (!batch) throw new Error(`unknown batch: ${id}`);
            this.state.batch = batch;
            this.state.counts = { done: 0, failed: 0 };
            return this.compose(true, [{ type: "data" }]) as unknown as T;
          }
          case "end_shift":
            return { counts: { ...this.state.counts } } as unknown as T;
          case "bridge_attached":
          case "heartbeat":
          case "log_console":
            return undefined as unknown as T;
          default:
            throw new Error(`fake backend: unknown command ${cmd}`);
        }
      },
    };
  }
}
