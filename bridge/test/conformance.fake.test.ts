// The vendored conformance suite against the REAL adapter + fake wire
// backend. Fast inner loop only — the in-webview run (`npm run conformance`)
// remains the sole merge gate.

import { describe, expect, it } from "vitest";
import { assertAllPassed, runBridgeConformanceTests } from "../vendor/conformance";
import { createStationAdapter } from "../src/adapter";
import { FakeBackend } from "./fakeBackend";

describe("vendored conformance suite vs real adapter (fake wire backend)", () => {
  it("passes with zero skips", async () => {
    const backend = new FakeBackend();
    const results = await runBridgeConformanceTests(
      () => {
        backend.reset();
        const adapter = createStationAdapter(backend.io(), backend.boot());
        backend.bindPush(adapter.applyPush);
        return adapter.bridge;
      },
      {
        fireTap: (result) => backend.fireTap(result),
        applyBatch: (id) => backend.applyBatch(id),
        settleMs: 0,
      },
    );
    expect(results.filter((r) => r.status === "skipped")).toEqual([]);
    expect(() => assertAllPassed(results)).not.toThrow();
    expect(results.every((r) => r.status === "passed")).toBe(true);
  });
});
