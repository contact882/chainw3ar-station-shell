// The conformance gate page. Loads in the REAL "main" window with the REAL
// production init script against the REAL Rust backend — passing here means
// the wiring is correct by construction. Both harness triggers are supplied,
// so zero tests skip; the Rust side treats any skip as a gate failure anyway.

import type { ConformanceResult } from "../vendor/conformance";
import { runBridgeConformanceTests } from "../vendor/conformance";
import type { StationBridge, TapResult } from "../vendor/contract";

interface TauriInternals {
  invoke<T>(cmd: string, args?: unknown): Promise<T>;
}

declare global {
  interface Window {
    __TAURI_INTERNALS__?: TauriInternals;
    __STATION_HARNESS__?: { reinstall(): unknown };
  }
}

const invoke = <T,>(cmd: string, args?: unknown): Promise<T> => {
  const internals = window.__TAURI_INTERNALS__;
  if (!internals) throw new Error("no __TAURI_INTERNALS__");
  return internals.invoke<T>(cmd, args ?? {});
};

function render(results: ConformanceResult[]): void {
  const rows = results
    .map((r) => {
      const color = r.status === "passed" ? "#2ed86a" : r.status === "skipped" ? "#d8a02e" : "#d82e4a";
      const detail = r.detail ? `<div class="detail">${escapeHtml(r.detail)}</div>` : "";
      return `<li style="color:${color}"><b>${r.status.toUpperCase()}</b> ${escapeHtml(r.name)}${detail}</li>`;
    })
    .join("");
  const failed = results.filter((r) => r.status === "failed").length;
  const skipped = results.filter((r) => r.status === "skipped").length;
  const verdict = failed === 0 && skipped === 0 ? "GATE: PASS" : "GATE: FAIL";
  document.body.innerHTML = `<h1>${verdict}</h1><ul>${rows}</ul>`;
}

function escapeHtml(s: string): string {
  return s.replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);
}

async function main(): Promise<void> {
  const results: ConformanceResult[] = [];
  try {
    const bridge = window.__STATION__ as StationBridge | undefined;
    if (!bridge) throw new Error("window.__STATION__ absent — injection failed");
    if ("controls" in (bridge as object)) {
      throw new Error("bridge carries a 'controls' key — the UI's mock duck-check would fault-screen");
    }
    const harness = window.__STATION_HARNESS__;
    if (!harness) throw new Error("__STATION_HARNESS__ absent — page must load with ?harness=1");

    results.push(
      ...(await runBridgeConformanceTests(
        async () => {
          await invoke("conformance_reset");
          return harness.reinstall() as StationBridge;
        },
        {
          fireTap: (result: TapResult) => invoke<void>("conformance_fire_tap", { verdict: result }),
          applyBatch: (id: string) => invoke<void>("apply_batch", { id }),
          settleMs: 200,
        },
      )),
    );
  } catch (err) {
    results.push({ name: "harness bootstrap", status: "failed", detail: String(err) });
  }
  render(results);
  try {
    await invoke("conformance_report", { results });
  } catch (err) {
    document.body.innerHTML += `<p>report failed: ${escapeHtml(String(err))}</p>`;
  }
}

void main();
