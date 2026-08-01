// Sim console page logic. Talks to Rust via invoke only — this window has NO
// bridge, no subscriptions, no audio (exactly one UI window per bridge; this
// is not a UI window).

export {}; // module scope — required for the global augmentation below

interface TauriInternals {
  invoke<T>(cmd: string, args?: unknown): Promise<T>;
}
declare global {
  interface Window {
    __TAURI_INTERNALS__?: TauriInternals;
  }
}

interface SimState {
  connection: string;
  readerPresent: boolean;
  downstreamReady: boolean;
  batchName: string | null;
  counts: { done: number; failed: number };
  pendingEndShift: boolean;
  seq: number;
  stickyChip: string | null;
  forcedNext: string | null;
  latencyMs: number;
}

interface Batch {
  id: string;
  name: string;
  total: number;
  completed: number;
  failed: number;
}

const toastEl = document.getElementById("toast")!;
const invoke = <T,>(cmd: string, args?: unknown): Promise<T> =>
  window.__TAURI_INTERNALS__!.invoke<T>(cmd, args ?? {}).catch((err: unknown) => {
    toastEl.textContent = `${cmd}: ${String(err)}`;
    throw err;
  });

const on = (id: string, fn: () => void): void => {
  document.getElementById(id)!.addEventListener("click", () => {
    toastEl.textContent = "";
    fn();
  });
};

let sticky = false;

on("tap-good", () => void invoke("sim_present_chip", { forced: "keyed" }));
on("tap-retry", () => void invoke("sim_present_chip", { forced: "recoverable" }));
on("tap-dead", () => void invoke("sim_present_chip", { forced: "permanent" }));
on("tap-random", () => void invoke("sim_present_chip", {}));
on("sticky", () => {
  sticky = !sticky;
  const btn = document.getElementById("sticky")!;
  btn.textContent = sticky
    ? "SAME-GARMENT MODE: ON (re-taps hit one garment)"
    : "SAME-GARMENT MODE: OFF (3 retries → failed)";
  void invoke("sim_arm_policy", { patch: { stickyChip: sticky ? "sim-sticky-demo" : "" } });
});

on("reader-off", () => void invoke("sim_set_reader", { present: false }));
on("reader-on", () => void invoke("sim_set_reader", { present: true }));
on("down-off", () => void invoke("sim_set_downstream", { ready: false }));
on("down-on", () => void invoke("sim_set_downstream", { ready: true }));
on("blip", () => void invoke("sim_blip", { ms: 2000 }));

const pick = document.getElementById("batch-pick") as HTMLSelectElement;
on("apply", () => void invoke("apply_batch", { id: pick.value }));
on("swap", () => void invoke("sim_swap_batch", { id: pick.value }));
on("end-shift", () => void invoke("sim_end_shift", {}));
on("quit", () => void invoke("sim_quit", {}));

async function loadBatches(): Promise<void> {
  const batches = await invoke<Batch[]>("list_batches");
  pick.innerHTML = batches
    .map((b) => `<option value="${b.id}">${b.name} (${b.completed}/${b.total})</option>`)
    .join("");
}

async function poll(): Promise<void> {
  try {
    const s = await invoke<SimState>("sim_get_state");
    document.getElementById("status")!.textContent =
      `line     ${s.connection.toUpperCase()}  (reader ${s.readerPresent ? "present" : "absent"}, ` +
      `ready ${s.downstreamReady ? "yes" : "no"})\n` +
      `batch    ${s.batchName ?? "—"}\n` +
      `counts   done ${s.counts.done} / failed ${s.counts.failed}\n` +
      `mode     ${s.stickyChip ? "same-garment" : "fresh garment per tap"}` +
      `${s.forcedNext ? `, armed: ${s.forcedNext}` : ""}, delay ${s.latencyMs}ms\n` +
      `endshift ${s.pendingEndShift ? "PENDING (re-emitting)" : "—"}   seq ${s.seq}`;
  } catch {
    // toast already shows the error
  }
}

void loadBatches();
void poll();
window.setInterval(() => void poll(), 500);
