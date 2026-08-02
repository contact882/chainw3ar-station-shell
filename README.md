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

**The primary dev escape is `station-shell.exe --quit` — no keyboard in the
loop, so it is the only route that works in every access mode.** The chord
(below) is the convenience for someone physically at the machine.

**REMOTE ACCESS — the chord does not survive remote desktop.** Observed
2026-08-01: Ctrl+Shift+F12 failed over AnyDesk — remote-desktop clients
(AnyDesk, RDP, TeamViewer, VNC) commonly intercept modifier+function chords
before they reach the machine. Not a shell defect (the chord is proven on a
physical keyboard at the machine); it is unavailable BY CONSTRUCTION to a
remote supervisor. For remote sessions the escape is `--quit`, with one
session rule, verified/analyzed 2026-08-01:

- `--quit` finds the station via a session-local mutex + window message
  (single-instance IPC) — it only sees a station running in the SAME Windows
  session as the shell it is typed into.
- A terminal opened through a screen-sharing client (AnyDesk-style) runs in
  the console session — same session as the kiosk → works. **VERIFIED live
  while an AnyDesk session was active on this machine** (sender exit 0,
  station exited, administrative-quit logged, 23:44Z).
- A shell from RDP-as-a-different-user, SSH, PsExec, or a session-0
  management agent is a DIFFERENT session → the single-instance transport
  cannot reach the station. **Source-proven (non-Global mutex; window
  messages cannot cross sessions), empirically UNVERIFIED on this machine
  (needs an elevated second session).**
- **Exit codes (CHA-54 item 7, shipped + gate-proven):** before exiting,
  the sender scans processes system-wide (process lists DO cross sessions).
  **Exit 3** = no station-shell process anywhere on this machine. **Exit 4**
  = a process named station-shell.exe exists but `--quit` cannot reach it —
  **name match only**: it does not confirm which build it is or whether it
  is healthy, and the sender's own message says so. On exit 4, use the
  elevated `taskkill` below. Manual cross-check remains
  `tasklist /fi "imagename eq station-shell.exe"`. The gate proves exit 4
  via a same-session unreachable instance; the cross-session variant is
  expected to exit 4 but stays UNVERIFIED (same elevation limit as above).
- Cross-session remote kill is `taskkill /f /im station-shell.exe` from an
  elevated shell (session-independent). **Verified live 2026-08-02**:
  force-kill of an armed running station left `session.json` byte-identical
  (SHA-256 readback), relaunch resumed the persisted shift, clean exit
  after — a force-kill is not a clean shutdown, and the atomic-persist
  guarantee held under readback, not assumption.

Since incident #2 (2026-08-01: an armed chord failed in the field and left
zero evidence), **arming is a proof, not a claim**: every armed launch
registers the chord, injects a synthetic TAP of that same chord through the
full OS→pump→handler chain, and only then logs
`dev exit chord armed (delivery verified)`. If the tap never comes back the
launch REFUSES to run (exit 71); if another process holds the chord, the
launch aborts with no "armed" line at all. The canary proves every software
layer — it cannot prove a physical keyboard can form the chord. That is
`--exit-probe`'s job: step 1 re-proves the synthetic chain, step 2 demands a
REAL 2-second hold (run it once per machine/keyboard; exit code is the
verdict).

**HARDWARE FINDING — why the chord is 3 keys held, not 4 keys (settled, do
not re-litigate):** proven as a controlled pair on the reference machine,
2026-08-01, **same keyboard, same session, ~27 minutes apart**:

- 4-key **Ctrl+Alt+Shift+Q** — `--exit-probe` step 1 (synthetic) **PASS**,
  step 2 (physical press) **FAIL** (~23:06Z).
- 3-key **Ctrl+Shift+F12 held 2s** — step 1 **PASS**, step 2 (physical hold)
  **PASS** (23:33:32Z, founder's hands).

Synthetic delivery passing both times proves the software chain was never
broken; the only changed variable is chord shape — the membrane keyboard
cannot form 4 simultaneous keys (rollover limit), and the 09:16 lockout that
morning was exactly that. Not machine, not software, not timing: chord
shape. The chord is now
**Ctrl+Shift+F12 held for 2 full seconds**: two modifiers + one key is the
most rollover-safe chord shape there is (the standard app-shortcut shape),
no Alt avoids AltGr collisions on international layouts, and accidental
presses are resisted twice — spatially (bottom-left modifier pair + top-right
F12: a brushing or resting gloved hand produces adjacent clusters, not
sustained opposite-corner triples) and temporally (a sub-2s press does
nothing — gate-pinned — and at fire time the shell re-checks the keys are
physically down, so a transient ghost or lost release can never fire it).
Anyone proposing a 4-key chord again: this paragraph is the answer.

| Route | Status on this machine |
|---|---|
| **`station-shell.exe --quit`** (terminal in the station's session) | **PRIMARY. EXITS the running instance cleanly — gate-verified 2026-08-01**, and live-verified during an active AnyDesk session, when that instance armed dev-exit; **refused + logged in lockdown posture** (also gate-verified). Exit 3 = no station anywhere (scan-verified); exit 4 = unreachable station-shell process exists, name match only (see remote note; both gate-proven). No flag memory, no keyboard, works in every access mode. |
| **Ctrl+Shift+F12 held 2s** (physical keyboard at the machine; debug build, no flags) | **EXITS cleanly — gate-verified 2026-08-01.** Debug builds arm by default, so `cargo run` is never a trap; `--no-dev-exit` disarms deliberately. NOT usable over remote-desktop clients (see remote note). |
| **Ctrl+Shift+F12 held 2s** (physical keyboard at the machine; release + `--dev-exit`) | **EXITS cleanly — gate-verified 2026-08-01**, all four postures (windowed/kiosk × sim/no-sim), synthetic input. A sub-hold tap does NOT exit (gate case 0). Physical proof per keyboard: `--exit-probe`. NOT usable over remote-desktop clients (see remote note). |
| QUIT SHELL button (`--sim`) | **EXITS cleanly — VERIFIED.** Lives in a fixed footer, always on-screen (incident #2: it sat below the fold of the 720px sim window; vitest-pinned). In kiosk+sim the console can still be BURIED behind the fullscreen kiosk once you touch the operator surface — reach for `--quit` or the chord instead of hunting windows. |
| Ctrl+Shift+F12 (release, no flag) | **Does nothing — gate-verified.** That's the lockdown; the startup WARN marks the trap in the log. |
| Alt+F4 | **Does nothing — VERIFIED** (close-prevented by design). |
| Ctrl+Shift+Esc | **Task Manager opens BEHIND the kiosk — VERIFIED.** Blind; do not rely on it. |
| Ctrl+Alt+Del → Sign out / Win+Ctrl+D | **UNVERIFIED.** Both failed in the field; do not rely on them. |
| `taskkill /f /im station-shell.exe` | Works **from an existing elevated shell, cross-session — VERIFIED 2026-08-02 with state readback** (session.json byte-identical after force-kill; shift resumed on relaunch). The only route that survives every remote closure. Prefer `--quit` where same-session — it's graceful and logged. |

- **Standard dev/integration launch: `station-shell.exe --dev-exit --sim`**
  (or plain `cargo run`, which is debug and therefore armed).
- While a dev-exit instance runs, Ctrl+Shift+F12 is captured system-wide on
  that machine (RegisterHotKey) — expected during dev sessions only.
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
