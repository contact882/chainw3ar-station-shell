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
cargo run -- --config path\station.toml
```

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
