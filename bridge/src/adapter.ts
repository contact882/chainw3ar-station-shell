// The injected StationBridge adapter. Pure logic, IO injected — vitest runs
// this same module against a fake wire backend; production wires it to Tauri
// invoke + the synchronous boot pull (see init.ts).
//
// Contract-critical invariants (see bridge/vendor/contract.ts, conformance.ts):
// - getSnapshot is synchronous; SAME reference while nothing changed, a NEW
//   deep-frozen object when something did. Old snapshots are never mutated.
// - applyPush swaps the snapshot cache BEFORE dispatching events, so a
//   tap-result listener calling getSnapshot() already sees the new counts.
// - `data`/`apply-batch` arrive snapshot-less on the wire; the adapter
//   substitutes its cache so event.snapshot === getSnapshot().
// - seq is monotonic; stale/duplicate payloads (eval vs invoke-response race,
//   evals landing across a reload boundary) are dropped by seq.
// - The bridge object must NEVER carry a `controls` key — the UI duck-types
//   its internal mock on that key and ?shell=1 would fault-screen.

import type {
  BatchInfo,
  ConnectionState,
  SessionCounts,
  StationBridge,
  StationEvent,
  StationSnapshot,
  TapResult,
} from "../vendor/contract";
import { CONTRACT_VERSION } from "../vendor/contract";

export interface AdapterIo {
  invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T>;
}

export type WireEvent =
  | { type: "tap-result"; result: TapResult }
  | { type: "connection"; state: ConnectionState }
  | { type: "data" }
  | { type: "apply-batch" }
  | { type: "end-shift" };

export interface WirePush {
  seq: number;
  snapshot?: StationSnapshot;
  events: WireEvent[];
}

export interface BootPayload {
  seq: number;
  snapshot: StationSnapshot;
  strings?: Record<string, string>;
}

interface EndShiftWire {
  counts: SessionCounts;
  payload?: WirePush;
}

export interface StationAdapter {
  bridge: StationBridge;
  applyPush(payload: WirePush): void;
  readonly lastSeq: number;
}

function deepFreeze<T>(value: T): T {
  if (value !== null && typeof value === "object") {
    for (const child of Object.values(value as object)) deepFreeze(child);
    Object.freeze(value);
  }
  return value;
}

export function createStationAdapter(io: AdapterIo, boot: BootPayload): StationAdapter {
  let lastSeq = boot.seq;
  let cache: StationSnapshot = deepFreeze(boot.snapshot);
  // Unique token per subscription: unsubscribe is idempotent AND cannot
  // detach a later subscription that reused the same function reference
  // (React StrictMode re-subscribes in dev builds of the UI).
  const tokens = new Set<{ fn: (event: StationEvent) => void }>();

  const expand = (wire: WireEvent): StationEvent => {
    if (wire.type === "data" || wire.type === "apply-batch") {
      return Object.freeze({ type: wire.type, snapshot: cache }) as StationEvent;
    }
    return Object.freeze({ ...wire }) as StationEvent;
  };

  const applyPush = (payload: WirePush): void => {
    if (
      payload === null ||
      typeof payload !== "object" ||
      typeof payload.seq !== "number" ||
      payload.seq <= lastSeq
    ) {
      return; // stale or duplicate — the other delivery channel won the race
    }
    lastSeq = payload.seq;
    if (payload.snapshot) cache = deepFreeze(payload.snapshot); // swap FIRST
    for (const wire of payload.events ?? []) {
      const event = expand(wire);
      for (const token of [...tokens]) {
        try {
          token.fn(event);
        } catch (err) {
          console.error("station-shell listener fault", err);
        }
      }
    }
  };

  // NOTE: listBatches results are fresh deserializations per call (defensive
  // copies by construction) and deliberately NOT frozen — the conformance
  // suite mutates a returned batch to prove the bridge can't be corrupted.
  const bridge = Object.freeze({
    contractVersion: CONTRACT_VERSION,
    getSnapshot: (): StationSnapshot => cache,
    subscribe(listener: (event: StationEvent) => void): () => void {
      const token = { fn: listener };
      tokens.add(token);
      let done = false;
      return () => {
        if (done) return;
        done = true;
        tokens.delete(token);
      };
    },
    listBatches: (): Promise<BatchInfo[]> => io.invoke<BatchInfo[]>("list_batches"),
    async selectBatch(id: string): Promise<void> {
      applyPush(await io.invoke<WirePush>("select_batch", { id }));
    },
    async endShift(): Promise<SessionCounts> {
      const reply = await io.invoke<EndShiftWire>("end_shift");
      if (reply.payload) applyPush(reply.payload);
      return reply.counts;
    },
  } satisfies StationBridge);

  // Type-level tripwire: adding a `controls` key must not compile.
  type NoControls = "controls" extends keyof typeof bridge ? never : true;
  const _noControls: NoControls = true;
  void _noControls;

  return {
    bridge,
    applyPush,
    get lastSeq() {
      return lastSeq;
    },
  };
}
