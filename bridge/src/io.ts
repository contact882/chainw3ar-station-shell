// Production IO: Tauri invoke (via __TAURI_INTERNALS__, which Tauri's own
// initialization scripts install BEFORE user init scripts run) and the
// synchronous boot pull against the station:// custom protocol.

import type { AdapterIo, BootPayload } from "./adapter";

interface TauriInternals {
  invoke<T>(cmd: string, args?: unknown): Promise<T>;
}

declare global {
  interface Window {
    __TAURI_INTERNALS__?: TauriInternals;
  }
}

export function internals(): TauriInternals {
  const found = window.__TAURI_INTERNALS__;
  if (!found) throw new Error("station-shell: __TAURI_INTERNALS__ missing (init script ran outside Tauri?)");
  return found;
}

export function tauriIo(): AdapterIo {
  const tauri = internals();
  return {
    invoke: <T,>(cmd: string, args?: Record<string, unknown>): Promise<T> =>
      tauri.invoke<T>(cmd, args ?? {}).catch((reason: unknown) => {
        throw reason instanceof Error ? reason : new Error(String(reason));
      }),
  };
}

/**
 * SYNCHRONOUS boot pull — the load-bearing call of the whole recovery design.
 * Init scripts are fixed strings that re-run on every document creation
 * (including the UI error boundary's location.reload() hours into a shift), so
 * the current snapshot must be fetched here, not baked at window creation.
 * The Rust handler is an ArcSwap read returning pre-serialized bytes; only the
 * renderer's JS thread blocks, never the host.
 */
export function fetchBootSync(): BootPayload | null {
  try {
    const xhr = new XMLHttpRequest();
    xhr.open("GET", "http://station.localhost/boot", false);
    xhr.send(null);
    if (xhr.status === 200) return JSON.parse(xhr.responseText) as BootPayload;
  } catch {
    // fall through to the baked fallback in init.ts
  }
  return null;
}
