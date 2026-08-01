// The Tauri initialization script (bundled to a single IIFE, injected into
// the "main" window ONLY — exactly one UI window per bridge, and the sim
// console never gets a bridge).
//
// Runs on EVERY document creation, including the UI error boundary's
// location.reload() hours into a shift. Order matters:
//   1. same-document double-injection guard
//   2. console forwarding (so even boot failures reach shell logs)
//   3. boot snapshot: sync station://boot pull → baked fallback → safe default
//   4. adapter construction; __STATION__ / __STATION_PUSH__ installed
//      non-writable, non-configurable
//   5. bridge_attached announce (Rust pushes a correction iff truth moved)
//   6. heartbeat; harness hook only under ?harness=1

import type { BootPayload, StationAdapter, WirePush } from "./adapter";
import { createStationAdapter } from "./adapter";
import { fetchBootSync, internals, tauriIo } from "./io";

declare global {
  interface Window {
    __STATION_SHELL_INIT__?: boolean;
    __STATION_PUSH__?: (payload: WirePush) => void;
    __STATION_STRINGS__?: Record<string, string>;
    __STATION_HARNESS__?: { reinstall(): unknown };
  }
}

(() => {
  if (window.__STATION_SHELL_INIT__) return;
  window.__STATION_SHELL_INIT__ = true;

  installConsoleForward();

  let io;
  try {
    io = tauriIo();
  } catch (err) {
    console.error("station-shell fault: no Tauri runtime — bridge not installed", err);
    return; // ?shell=1 will fault-screen; the watchdog owns recovery
  }

  // ── Boot snapshot: current truth, synchronously, on every document ──────
  let boot: BootPayload | null = fetchBootSync();
  if (!boot) {
    console.error("station-shell fault: boot pull failed — using baked snapshot (may be stale)");
    try {
      boot = JSON.parse("__BAKED_BOOT_JSON__") as BootPayload;
    } catch {
      boot = null;
    }
  }
  if (!boot) {
    // Last resort: block the line rather than lie about state.
    boot = {
      seq: 0,
      snapshot: {
        connection: "disconnected",
        currentBatch: null,
        sessionCounts: { done: 0, failed: 0 },
        config: { failBinSide: "right" },
      },
    };
  }

  let current: StationAdapter = createStationAdapter(io, boot);

  Object.defineProperty(window, "__STATION__", {
    value: current.bridge,
    writable: false,
    configurable: false,
    enumerable: true,
  });
  Object.defineProperty(window, "__STATION_PUSH__", {
    // Routes to the NEWEST adapter — the conformance factory reinstalls one
    // per test and stale adapters must go dead.
    value: (payload: WirePush) => current.applyPush(payload),
    writable: false,
    configurable: false,
    enumerable: false,
  });
  if (boot.strings) {
    window.__STATION_STRINGS__ = boot.strings;
  }

  // Announce with the seq we booted from; Rust replies with a corrective push
  // only if its truth has moved past it (covers the baked-fallback path).
  io.invoke("bridge_attached", { adapterSeq: current.lastSeq }).catch(() => {});

  window.setInterval(() => {
    io.invoke("heartbeat").catch(() => {});
  }, 5000);

  if (new URLSearchParams(window.location.search).get("harness") === "1") {
    window.__STATION_HARNESS__ = {
      reinstall() {
        const fresh = fetchBootSync();
        if (!fresh) throw new Error("harness reinstall: boot pull failed");
        current = createStationAdapter(io, fresh);
        return current.bridge;
      },
    };
  }
})();

function installConsoleForward(): void {
  let forwarding = false;
  let budget = 200;
  window.setInterval(() => {
    budget = 200;
  }, 60_000);
  for (const level of ["error", "warn"] as const) {
    const original = console[level].bind(console);
    console[level] = (...args: unknown[]) => {
      original(...args);
      if (forwarding || budget-- <= 0) return;
      forwarding = true;
      try {
        const message = args
          .map((a) => (a instanceof Error ? `${a.name}: ${a.message}` : String(a)))
          .join(" ");
        internals().invoke("log_console", { level, message }).catch(() => {});
      } catch {
        // console must never throw
      } finally {
        forwarding = false;
      }
    };
  }
}
