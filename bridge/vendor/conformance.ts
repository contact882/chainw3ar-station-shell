// Executable contract conformance suite for StationBridge implementations.
//
// PURPOSE: the shell repo copies this file (together with contract.ts —
// both are dependency-free) and runs it against its own JS adapter in its
// test harness. Every assertion here is a semantic the UI relies on; a
// bridge that passes wires mechanically, a bridge that fails would have
// produced a silent runtime mismatch.
//
// Framework-agnostic: no test-runner imports. Returns structured results;
// call `assertAllPassed` to throw on any failure (usable as a merge gate).
//
// Tap-verdict semantics require a way to trigger taps, which the contract
// deliberately does not expose (taps come from hardware). Provide
// `options.fireTap` from your harness (the simulated reader path); without
// it, tap assertions are reported as skipped, not passed.

import type { StationBridge, StationEvent, TapResult } from "./contract";
import { CONTRACT_VERSION } from "./contract";

export interface ConformanceResult {
  name: string;
  status: "passed" | "failed" | "skipped";
  detail?: string;
}

export interface ConformanceOptions {
  /** Harness-provided tap trigger (e.g. the simulated reader). */
  fireTap?: (result: TapResult) => void | Promise<void>;
  /** Harness-provided shell-side shift begin (apply-batch emitter). */
  applyBatch?: (id: string) => void | Promise<void>;
  /** Milliseconds to wait after async triggers before asserting. Default 50. */
  settleMs?: number;
}

type BridgeFactory = () => StationBridge | Promise<StationBridge>;

const wait = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

function assert(condition: boolean, detail: string): asserts condition {
  if (!condition) throw new Error(detail);
}

export async function runBridgeConformanceTests(
  createBridge: BridgeFactory,
  options: ConformanceOptions = {},
): Promise<ConformanceResult[]> {
  const settleMs = options.settleMs ?? 50;
  const results: ConformanceResult[] = [];

  const test = async (name: string, run: () => Promise<void> | void): Promise<void> => {
    try {
      await run();
      results.push({ name, status: "passed" });
    } catch (error) {
      results.push({ name, status: "failed", detail: String(error) });
    }
  };

  const skip = (name: string, detail: string): void => {
    results.push({ name, status: "skipped", detail });
  };

  await test("contractVersion matches the UI's CONTRACT_VERSION", async () => {
    const bridge = await createBridge();
    assert(
      bridge.contractVersion === CONTRACT_VERSION,
      `expected ${CONTRACT_VERSION}, got ${bridge.contractVersion}`,
    );
  });

  await test("getSnapshot is synchronous, complete, and stable when unchanged", async () => {
    const bridge = await createBridge();
    const a = bridge.getSnapshot();
    assert(a !== null && typeof a === "object", "snapshot must be an object");
    assert(
      a.connection === "connected" || a.connection === "disconnected",
      `connection must be the literal "connected" or "disconnected", got ${String(a.connection)}`,
    );
    assert("currentBatch" in a, "snapshot must carry currentBatch (null when no shift)");
    assert(
      typeof a.sessionCounts?.done === "number" && typeof a.sessionCounts?.failed === "number",
      "sessionCounts must carry numeric done/failed",
    );
    assert(
      a.config?.failBinSide === "left" || a.config?.failBinSide === "right",
      `config.failBinSide must be "left" or "right", got ${String(a.config?.failBinSide)}`,
    );
    const b = bridge.getSnapshot();
    assert(a === b, "unchanged data must return the same snapshot reference");
  });

  await test("listBatches returns well-formed, defensively-copied batches", async () => {
    const bridge = await createBridge();
    const batches = await bridge.listBatches();
    assert(Array.isArray(batches) && batches.length > 0, "must return a non-empty array");
    for (const batch of batches) {
      assert(typeof batch.id === "string" && batch.id.length > 0, "batch.id must be a string");
      assert(typeof batch.name === "string", "batch.name must be a string");
      for (const key of ["total", "completed", "failed"] as const) {
        assert(typeof batch[key] === "number", `batch.${key} must be a number`);
      }
    }
    (batches[0] as { completed: number }).completed = 999_999;
    const again = await bridge.listBatches();
    assert(again[0].completed !== 999_999, "mutating a returned batch must not corrupt the bridge");
  });

  await test("selectBatch activates the batch, resets counts, emits data", async () => {
    const bridge = await createBridge();
    const events: StationEvent[] = [];
    bridge.subscribe((event) => events.push(event));
    const [first] = await bridge.listBatches();
    const before = bridge.getSnapshot();
    await bridge.selectBatch(first.id);
    await wait(settleMs);
    const after = bridge.getSnapshot();
    assert(after !== before, "snapshot reference must change");
    assert(after.currentBatch?.id === first.id, "currentBatch must be the selected batch");
    assert(
      after.sessionCounts.done === 0 && after.sessionCounts.failed === 0,
      "selectBatch is the ONLY counts reset — and it must reset",
    );
    assert(
      events.some((event) => event.type === "data"),
      "a data event must be emitted",
    );
  });

  await test("selectBatch rejects an unknown id", async () => {
    const bridge = await createBridge();
    let rejected = false;
    try {
      await bridge.selectBatch("__conformance_nonexistent__");
    } catch {
      rejected = true;
    }
    assert(rejected, "unknown batch id must reject");
  });

  await test("endShift resolves counts and resets nothing", async () => {
    const bridge = await createBridge();
    const [first] = await bridge.listBatches();
    await bridge.selectBatch(first.id);
    const counts = await bridge.endShift();
    assert(typeof counts.done === "number" && typeof counts.failed === "number", "must resolve counts");
    const snapshot = bridge.getSnapshot();
    assert(
      snapshot.sessionCounts.done === counts.done && snapshot.sessionCounts.failed === counts.failed,
      "endShift must not reset the session",
    );
  });

  await test("unsubscribe stops delivery", async () => {
    const bridge = await createBridge();
    const events: StationEvent[] = [];
    const unsubscribe = bridge.subscribe((event) => events.push(event));
    unsubscribe();
    const [first] = await bridge.listBatches();
    await bridge.selectBatch(first.id);
    await wait(settleMs);
    assert(events.length === 0, "no events after unsubscribe");
  });

  const { applyBatch } = options;
  if (!applyBatch) {
    skip("apply-batch: assigns batch, resets counts, emits with snapshot", "no applyBatch provided by harness");
  } else {
    await test("apply-batch: assigns batch, resets counts, emits with snapshot", async () => {
      const bridge = await createBridge();
      const events: StationEvent[] = [];
      bridge.subscribe((event) => events.push(event));
      const [target] = await bridge.listBatches();
      await applyBatch(target.id);
      await wait(settleMs);
      const snapshot = bridge.getSnapshot();
      assert(snapshot.currentBatch?.id === target.id, "currentBatch must be the applied batch");
      assert(
        snapshot.sessionCounts.done === 0 && snapshot.sessionCounts.failed === 0,
        "apply-batch begins a shift — counts must be fresh",
      );
      const applied = events.find((event) => event.type === "apply-batch");
      assert(applied !== undefined, "an apply-batch event must be emitted");
      assert(
        applied.type === "apply-batch" && applied.snapshot.currentBatch?.id === target.id,
        "the event's snapshot must already carry the applied batch",
      );
    });
  }

  const { fireTap } = options;
  if (!fireTap) {
    skip("success verdict: counts, ordering, immutability", "no fireTap provided by harness");
    skip("retry verdict: nothing moves, no data event", "no fireTap provided by harness");
    skip("dead verdict: failed counts move", "no fireTap provided by harness");
  } else {
    await test("success verdict: counts, ordering, immutability", async () => {
      const bridge = await createBridge();
      const [first] = await bridge.listBatches();
      await bridge.selectBatch(first.id);
      await wait(settleMs);
      const before = bridge.getSnapshot();
      const events: StationEvent[] = [];
      const seenAtTapResult: Array<ReturnType<StationBridge["getSnapshot"]>> = [];
      bridge.subscribe((event) => {
        events.push(event);
        if (event.type === "tap-result") seenAtTapResult.push(bridge.getSnapshot());
      });
      await fireTap("success");
      await wait(settleMs);
      const after = bridge.getSnapshot();
      assert(after.sessionCounts.done === before.sessionCounts.done + 1, "done must increment");
      assert(
        (after.currentBatch?.completed ?? 0) === (before.currentBatch?.completed ?? 0) + 1,
        "batch.completed must increment",
      );
      assert(after !== before, "snapshot reference must change");
      assert(
        before.sessionCounts.done === 0,
        "the old snapshot object must be untouched (immutability)",
      );
      const types = events.map((event) => event.type);
      assert(
        types.indexOf("tap-result") !== -1 && types.indexOf("tap-result") < types.indexOf("data"),
        `tap-result must precede data, got [${types.join(", ")}]`,
      );
      assert(
        seenAtTapResult[0]?.sessionCounts.done === before.sessionCounts.done + 1,
        "the snapshot must already reflect the tap when tap-result fires",
      );
    });

    await test("retry verdict: nothing moves, no data event", async () => {
      const bridge = await createBridge();
      const [first] = await bridge.listBatches();
      await bridge.selectBatch(first.id);
      await wait(settleMs);
      const before = bridge.getSnapshot();
      const events: StationEvent[] = [];
      bridge.subscribe((event) => events.push(event));
      await fireTap("retry");
      await wait(settleMs);
      const after = bridge.getSnapshot();
      assert(
        after.sessionCounts.done === before.sessionCounts.done &&
          after.sessionCounts.failed === before.sessionCounts.failed,
        "retry is not a verdict — counts must not move",
      );
      assert(
        events.some((event) => event.type === "tap-result"),
        "tap-result must still be emitted",
      );
      assert(
        !events.some((event) => event.type === "data"),
        "no data event when nothing changed",
      );
    });

    await test("dead verdict: failed counts move", async () => {
      const bridge = await createBridge();
      const [first] = await bridge.listBatches();
      await bridge.selectBatch(first.id);
      await wait(settleMs);
      const before = bridge.getSnapshot();
      await fireTap("dead");
      await wait(settleMs);
      const after = bridge.getSnapshot();
      assert(after.sessionCounts.failed === before.sessionCounts.failed + 1, "session failed must increment");
      assert(
        (after.currentBatch?.failed ?? 0) === (before.currentBatch?.failed ?? 0) + 1,
        "batch.failed must increment",
      );
    });
  }

  return results;
}

/** Throws a summarized error if any test failed — usable as a merge gate. */
export function assertAllPassed(results: ConformanceResult[]): void {
  const failed = results.filter((result) => result.status === "failed");
  if (failed.length > 0) {
    throw new Error(
      `${failed.length} conformance failure(s):\n` +
        failed.map((result) => `  ✕ ${result.name}: ${result.detail}`).join("\n"),
    );
  }
}
