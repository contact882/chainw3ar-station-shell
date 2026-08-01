// Adapter regressions the conformance suite doesn't pin down directly.

import { describe, expect, it } from "vitest";
import type { StationEvent } from "../vendor/contract";
import { createStationAdapter } from "../src/adapter";
import { FakeBackend } from "./fakeBackend";

function fresh() {
  const backend = new FakeBackend();
  const adapter = createStationAdapter(backend.io(), backend.boot());
  backend.bindPush(adapter.applyPush);
  return { backend, adapter };
}

describe("adapter", () => {
  it("never carries a `controls` key (the UI's mock duck-check)", () => {
    const { adapter } = fresh();
    expect("controls" in (adapter.bridge as object)).toBe(false);
    expect(Object.keys(adapter.bridge).sort()).toEqual([
      "contractVersion",
      "endShift",
      "getSnapshot",
      "listBatches",
      "selectBatch",
      "subscribe",
    ]);
  });

  it("drops stale and duplicate seq payloads (eval/invoke race)", async () => {
    const { backend, adapter } = fresh();
    await adapter.bridge.selectBatch("b1"); // seq 1 via invoke response
    const ref = adapter.bridge.getSnapshot();
    // The eval-channel duplicate of the same payload arrives late:
    adapter.applyPush({ seq: 1, snapshot: { ...ref, sessionCounts: { done: 99, failed: 99 } }, events: [{ type: "data" }] });
    expect(adapter.bridge.getSnapshot()).toBe(ref, );
    expect(adapter.bridge.getSnapshot().sessionCounts.done).toBe(0);
    void backend;
  });

  it("unsubscribe is idempotent and scoped to its own token (StrictMode)", () => {
    const { adapter } = fresh();
    const seen: StationEvent[] = [];
    const listener = (e: StationEvent) => seen.push(e);
    const un1 = adapter.bridge.subscribe(listener);
    un1();
    un1(); // double unsub of a dead token…
    const un2 = adapter.bridge.subscribe(listener); // …must not detach this one
    adapter.applyPush({ seq: 999, events: [{ type: "end-shift" }] });
    expect(seen.length).toBe(1);
    un2();
  });

  it("snapshot cache swaps before events dispatch; event.snapshot === getSnapshot()", () => {
    const { backend, adapter } = fresh();
    void backend;
    let seenAtDispatch: unknown = null;
    adapter.bridge.subscribe((e) => {
      if (e.type === "data") seenAtDispatch = e.snapshot === adapter.bridge.getSnapshot();
    });
    adapter.applyPush({
      seq: 50,
      snapshot: { ...adapter.bridge.getSnapshot(), sessionCounts: { done: 7, failed: 0 } },
      events: [{ type: "data" }],
    });
    expect(seenAtDispatch).toBe(true);
    expect(adapter.bridge.getSnapshot().sessionCounts.done).toBe(7);
  });

  it("snapshots are deeply frozen; listBatches results are not", async () => {
    const { adapter } = fresh();
    const snapshot = adapter.bridge.getSnapshot();
    expect(Object.isFrozen(snapshot)).toBe(true);
    expect(Object.isFrozen(snapshot.sessionCounts)).toBe(true);
    const batches = await adapter.bridge.listBatches();
    expect(Object.isFrozen(batches[0])).toBe(false); // conformance mutates them
  });

  it("a listener that throws does not break delivery to the others", () => {
    const { adapter } = fresh();
    const seen: string[] = [];
    adapter.bridge.subscribe(() => {
      throw new Error("bad listener");
    });
    adapter.bridge.subscribe((e) => seen.push(e.type));
    adapter.applyPush({ seq: 10, events: [{ type: "end-shift" }] });
    expect(seen).toEqual(["end-shift"]);
  });
});
