# chainw3ar-station-shell

Tauri kiosk shell for the Chainw3ar encoding-station operator console. Hosts
the **frozen** UI (chainw3ar-station-ui @ `v0.2.0`), injects the
`window.__STATION__` StationBridge (contract v1) via a pre-bundle init
script, and owns everything the UI deliberately does not: verdict emission,
per-chip retry counting (3rd consecutive recoverable failure → dead),
session persistence across crashes, batch state, and the meaning of
`connected`.

**Hard boundary:** keying, sealed-blob handling, and the guarded database
write live in a separate governed repository. This build drives SIMULATED
seams only — see `SEAM.md` for the exact interface the governed repo
implements. This repo contains zero APDUs; the PC/SC monitor is
presence-detection only.

## Prerequisites

Rust ≥1.77 (MSVC), Node ≥22 for the shell's own tooling, WebView2 runtime
(dev machines have Evergreen; production ships Fixed Version — see
`DEPLOYMENT.md`). The frozen UI builds with a portable Node 20.11.1 that
`npm run fetch-ui` downloads automatically (SHA-256-pinned, cached in
`tools/`).

## Build & gates

```powershell
npm install               # shell tooling (esbuild, typescript, vitest)
npm run fetch-ui          # clone UI @ v0.2.0, build dist with Node 20.11.1 → ui-dist/
npm run build:web         # init script → src-tauri/gen, harness+sim+UI → app-dist/
npm run conformance       # ← THE ACCEPTANCE GATE (details below)
npm test                  # pin gate + tsc + vitest inner loop
cargo test                # (in src-tauri) state actor, retry, persistence, pin mirror
powershell -File scripts\exit-gate.ps1   # escape-path gate: exits work, lockdown holds
```

**The acceptance gate** (`npm run conformance`) runs the UI's own
`runBridgeConformanceTests` inside the real shell webview against the real
Rust backend, with both harness triggers wired (forced-verdict taps through
the real pipeline + the real apply-batch path). It must report
**11 passed / 0 failed / 0 skipped**; skips count as failures. Exit code is
the verdict (0 pass, 1 fail, 2 harness-dead).

**Contract pin:** `bridge/vendor/{contract.ts,conformance.ts}` are byte
copies of UI tag `v0.2.0`, SHA-256-pinned in `bridge/vendor/PIN.json` and
enforced three ways (npm pin script, `cargo test` mirror, `build.rs` — a
drifted contract does not compile). Never edit the vendored files.

## Run modes

```powershell
cargo run                          # kiosk: fullscreen, always-on-top, close blocked
cargo run -- --windowed            # dev posture: 1280×720 + devtools
cargo run -- --windowed --sim      # + sim console window (the demo driver)
cargo run -- --conformance         # acceptance gate
cargo run -- --spike 20            # reload-freshness proof (see below)
cargo run -- --reader-probe        # guided hardware gate (see below)
cargo run -- --exit-probe          # guided exit-chord gate: synthetic + PHYSICAL press (see below)
station-shell.exe --quit           # ask a RUNNING instance to exit cleanly (see below)
cargo run -- --config path\station.toml
```

## Exiting kiosk mode (read this BEFORE launching kiosk on a dev box)

**A RELEASE kiosk launch without `--dev-exit` has NO local escape. Treat it
as reboot-only** (remote kill is the administrative path). Every gate-verified
row below is enforced by `scripts\exit-gate.ps1` — it launches the real exe in
every posture, injects the chord, and proves both the exits AND the lockdown.
Re-run it after any change to exits, windows, or shortcut plumbing; a row
without a gate case is UNVERIFIED by definition.

Since incident #2 (2026-08-01: an armed chord failed in the field and left
zero evidence), **arming is a proof, not a claim**: every armed launch
registers the chord plus a canary hotkey, drives a synthetic canary press
through the full OS→pump→handler chain, and only then logs
`dev exit chord armed (delivery verified)`. If the canary never comes back the
launch REFUSES to run (exit 71); if another process holds the chord, the
launch aborts with no "armed" line at all. The canary proves every software
layer — it cannot prove a physical keyboard can form the 4-key chord. That is
`--exit-probe`'s job: step 1 re-proves the synthetic chain, step 2 demands a
REAL press within 15s (run it once per machine/keyboard; exit code is the
verdict).

| Route | Status on this machine |
|---|---|
| **Ctrl+Alt+Shift+Q** (debug build, no flags) | **EXITS cleanly — gate-verified 2026-08-01.** Debug builds arm by default, so `cargo run` is never a trap; `--no-dev-exit` disarms deliberately. |
| **Ctrl+Alt+Shift+Q** (release + `--dev-exit`) | **EXITS cleanly — gate-verified 2026-08-01**, all four postures (windowed/kiosk × sim/no-sim), synthetic input. Physical-keyboard proof: `--exit-probe`. |
| **`station-shell.exe --quit`** (any terminal) | **EXITS the running instance cleanly — gate-verified 2026-08-01** when that instance armed dev-exit; **refused + logged in lockdown posture** (also gate-verified). Exit 3 when nothing is running. The dev exit that depends on no flag memory and no keyboard. |
| QUIT SHELL button (`--sim`) | **EXITS cleanly — VERIFIED.** Now lives in a fixed footer, always on-screen (incident #2: it sat below the fold of the 720px sim window; vitest-pinned). In kiosk+sim the console can still be BURIED behind the fullscreen kiosk once you touch the operator surface — reach for the chord or `--quit` instead of hunting windows. |
| Ctrl+Alt+Shift+Q (release, no flag) | **Does nothing — gate-verified.** That's the lockdown; the startup WARN marks the trap in the log. |
| Alt+F4 | **Does nothing — VERIFIED** (close-prevented by design). |
| Ctrl+Shift+Esc | **Task Manager opens BEHIND the kiosk — VERIFIED.** Blind; do not rely on it. |
| Ctrl+Alt+Del → Sign out / Win+Ctrl+D | **UNVERIFIED.** Both failed in the field; do not rely on them. |
| `taskkill /f /im station-shell.exe` | Works **from an existing shell** (VERIFIED). Remote access only on a bare kiosk. Prefer `--quit` on dev machines — it's graceful and logged. |

- **Standard dev/integration launch: `station-shell.exe --dev-exit --sim`**
  (or plain `cargo run`, which is debug and therefore armed).
- **Known residual:** one field failure of a physically pressed chord
  (2026-08-01 09:16, armed + delivery-healthy configuration) remains
  unreproduced — every synthetic layer passes the gate, so the physical input
  path is the suspect. `--exit-probe` on the affected keyboard is the
  discriminating test; `--quit` removes the keyboard from the loop entirely.
- **Production**: recovery is the supervisor runbook in `DEPLOYMENT.md`
  (self-recovery → remote administrative restart → power cycle); never put
  `--dev-exit` in a floor launch configuration. A production kiosk refuses
  `--quit` by design (founder decision, 2026-08-01).

**`--reader-probe`** verifies the detection path against the real reader with
automatic PASS/FAIL judging — no windows, no interpretation needed. It walks
you through the four hardware cases: (1) empty reader → no phantom
presentations; (2) present a card → fires exactly once, no re-fire while it
sits there; (3) remove it → clean idle, then a re-present fires again (edge
re-armed); (4) unplug the reader mid-run → graceful reader-absent, replug →
recovery without restart. Exit 0 only if every attempted case passes. The
monitor never connects to the card and never transmits — the probe prints
ATR byte *length* only.

- `station.toml` — failBinSide, PC/SC reader match, sim weights/latency.
- State lives in `%APPDATA%\com.chainw3ar.station\` (session.json,
  failures.jsonl, sim_progress.json, logs\shell.log.*,
  conformance-results.json).
- A physical tap on the station's reader (presence edge only — nothing is
  transmitted) drives the simulated keying pipeline exactly like the sim
  console's TAP buttons.
- **Sim identity limitation:** stubbed chip refs are `sha256(ATR ‖ tap
  timestamp)` — ATR is card-TYPE data shared by same-model chips, so in sim
  mode retry counting cannot distinguish two chips of the same type or
  recognize a re-tapped chip (use SAME-GARMENT MODE to demo escalation).
  Sim constraint only: real per-chip identity comes from the governed keying
  layer's `chip_ref` (SEAM.md §1.3).
- **Demo data accumulates by design** (batch progress is inventory-scoped
  and survives restarts) — the fixture CANVAS CAP deliberately ships at
  **499/500** so one success demos BATCH COMPLETE, which means demo sessions
  quickly show overruns like 525/500. That display is contract-honest (the
  UI never clamps; overshoot is information) and every overrun success is
  recorded in `overruns.jsonl`. To start a demo clean: sim console →
  **RESET DEMO DATA**. Production policy knob: `overrun_policy` in
  station.toml (`allow` = count + record, default; `block` = taps past total
  never reach keying).

## Why reloads are lossless (`--spike`)

The UI resolves `__STATION__` once at bundle eval and derives its screen from
a synchronous `getSnapshot()`. Init scripts are fixed strings that re-run on
every document creation — so the adapter **pulls the boot snapshot
synchronously** from `http://station.localhost/boot` (an ArcSwap read in the
shell) on every load, with a window-creation-baked snapshot only as fallback.
`--spike N` mutates state and reloads the document N times, asserting every
fresh document attached with current truth (`stale boots 0`). Run it after
any change to the init script, boot protocol, or window creation.

## Founder smoke checklist (pre-demo ritual)

0. New machine or new keyboard? `cargo run -- --exit-probe` once and press the
   chord when prompted — proves the keyboard and the whole exit chain.
1. `cargo run -- --windowed --sim` — UI boots to batch select; batch names
   are the fixture garments (mock names like "SS26 Denim Jacket" on screen
   mean injection failed).
2. Select a batch → READY + quiet start ding.
3. Tap a chip on the reader (or sim TAP buttons) → verdict flash + counts.
4. SAME-GARMENT MODE on → three TAP–RETRY in a row → third flashes FAILED.
5. Kill the process mid-shift (`taskkill /f /im station-shell.exe`),
   relaunch → resumes to READY with the same counts, no batch select.
6. BLIP → LINE PAUSED takeover + one alert tone; reconnect pings the dot.
7. ASSIGN BATCH from the sim console → operating screen, zeroed counts, ding.
8. END SHIFT hold (2.5s) → summary matches counts; NEW SHIFT (2s) → batch
   select.
9. `npm run conformance` → 11/0/0.

## Layout

```
bridge/vendor/     frozen contract.ts + conformance.ts + PIN.json (never edit)
bridge/src/        adapter + init script (TS → esbuild IIFE → src-tauri/gen/)
bridge/harness/    conformance gate page   bridge/sim-console/  sim console page
bridge/test/       vitest inner loop (real adapter + fake wire backend)
scripts/           check-contract-pin / fetch-ui / build-web
src-tauri/src/     core actor, seams (SEAM.md), reader monitor, boot protocol,
                   push (seq'd evals), persistence, watchdog, windows, commands
SEAM.md            the governed-repo interface contract
DEPLOYMENT.md      fixed-runtime, restart wrapper, GPU check, soak — deferred items
```
