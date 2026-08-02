# DEPLOYMENT.md — deferred deployment work, recorded so it isn't rediscovered

None of this blocks the simulated-keying milestone; all of it is required
before a factory floor deployment.

## Supervisor recovery runbook (what to do when a floor machine hangs)

In order. Each layer exists because the one above it can fail.

1. **Do nothing for 60 seconds.** Renderer-class hangs (frozen/black page)
   self-recover: heartbeat silence or a webview process failure triggers a
   reload/recreation, logged with its reason, capped at 3 per 60s.
2. **LINE PAUSED fault screen showing?** That is the shell refusing to
   flicker: the recovery cap was exceeded and it stopped trying. This state
   requires a deliberate process restart — remotely (next line) or by a
   supervisor with machine access. State is safe on disk; restart is
   lossless. Do NOT configure anything to auto-kill a fault-state process.
3. **Remote administrative restart — REQUIRED fleet capability, and the
   ONLY remote path BY CONSTRUCTION (2026-08-01 finding).** Every floor
   machine must have a remote path to kill/relaunch the station process
   (RDP, management agent, or remote service control). This is load-bearing,
   not a nice-to-have, because every alternative is structurally closed for
   a remote supervisor:
   - The exit chord is unavailable remotely twice over: it is never armed in
     production, and remote-desktop clients intercept modifier+function
     chords before they reach the machine (observed live over AnyDesk).
   - `--quit` is refused by the production posture (founder decision), and
     even where armed it is session-scoped: it only sees a station in the
     same Windows session as the shell it runs from (session-local mutex +
     window message). A session-0 agent or different-user RDP shell cannot
     reach it — expected exit 4 ("unreachable station-shell process, name
     match only"); cross-session behaviour is source-proven, empirically
     UNVERIFIED (needs an elevated second session).
   - What DOES work remotely and cross-session: `taskkill /f /im
     station-shell.exe` (or service control) from an elevated shell, then
     relaunch via the wrapper. **Verified live 2026-08-02:** force-kill of an
     armed running station left `session.json` byte-identical (SHA-256
     readback), still-valid JSON; relaunch logged `crash/restart recovery:
     resuming persisted shift`; clean exit after. (This kill happened while
     idle — the kill-DURING-a-write case rests on the atomic temp→fsync→rename
     design plus the earlier live mid-shift kills; worst case remains state
     as of the last completed tap.)
   A fleet without remote restart therefore has only layer 4 — and a power
   cycle during active keying is recorded chip-scrap risk until the governed
   layer certifies crash-safety (SEAM.md §1.3b). If §1.3b turns out unsafe,
   remote restart stops being infrastructure and becomes the SOLE safe
   recovery. Tracked as CHA-54 item 6.

   **Supervisor rule for `--quit` exit codes (CHA-54 item 7 — SHIPPED,
   gate-proven):** the sender scans processes system-wide before exiting.
   Exit 3 = no station-shell process anywhere on this machine. Exit 4 = a
   process named station-shell.exe exists but is unreachable from this
   session — NAME MATCH ONLY (does not confirm build or health; the
   sender's message says so): use the elevated `taskkill` above. Manual
   cross-check: `tasklist /fi "imagename eq station-shell.exe"`. The gate
   proves exit 4 with a same-session unreachable instance; the
   cross-session variant stays UNVERIFIED (elevation limit).
4. **Hard power cycle — the last resort, and acceptable.** The shell's state
   files are written atomically (temp → fsync → rename): a cut at any
   instant leaves old-or-new state, never a torn file; worst case is the
   state as of the last completed tap. `failures.jsonl`/`overruns.jsonl`
   may carry one torn final line — consumers must tolerate a torn tail.
   Chip-mid-key and DB-write-in-flight safety are governed-layer
   obligations (SEAM.md §1.3b) — until the governed layer certifies them,
   treat a power cycle during active keying as a chip-scrap risk.

## Fixed Version WebView2 runtime (the most consequential item)

Target machines are offline: the Evergreen runtime cannot be assumed present
or updatable (TAURI.md). Ship the **Fixed Version** runtime with the
installer: download the fixed-version package matching the validated
version, unpack beside the exe, and point the shell at it via
`WEBVIEW2_BROWSER_EXECUTABLE_FOLDER` (or tauri.conf
`bundle.windows.webviewInstallMode = { type: "fixedRuntime", path: … }` once
the NSIS bundle is enabled: `bundle.active = true`, add real icons). Pin one
runtime version per fleet; revalidate `--spike` and the conformance gate on
every runtime bump.

## Restart wrapper / autostart / fault state

The shell recovers webview failures itself (renderer reload / window
recreation), logging every recovery with its reason, capped at **3 in 60s**.
Past the cap it does NOT loop: it holds a persistent shell-owned LINE PAUSED
screen and stops recovering — a deliberate, visible fault state that requires
a human or remote service action to restart the process (state is on disk;
the restart is lossless). **Do not configure the wrapper to kill a
fault-state process automatically** — it is saying something is wrong.

**Launch-flag audit for floor machines:** the production launch command must
be exactly `station-shell.exe` (plus `--config` if used) — never `--dev-exit`
(enables a keyboard exit chord), `--sim`, `--windowed`, or any verification
flag. One-line check for the wrapper config before deployment.

**`--quit` posture (2026-08-01, founder decision):** `station-shell.exe
--quit` asks a running instance to exit cleanly, and a production kiosk
REFUSES it (logged: "quit refused — lockdown posture"); it is honored only by
instances that armed `--dev-exit`. It is also SESSION-SCOPED: it must run
from a shell in the same Windows session as the station (a screen-sharing
client's terminal qualifies — verified live over AnyDesk; a session-0 agent
or different-user RDP shell does not, and gets exit 3 with the station still
running). The administrative path for production remains remote
kill/relaunch (layer 3 above). The refusal is proven by
`scripts\exit-gate.ps1` case 6 — re-run the gate before every deployment
image change.

**Exit-path verification (dev/integration machines):** before trusting a dev
box's escape hatch, run `scripts\exit-gate.ps1` (automated: every armed
posture exits on the held chord, a sub-hold tap does not, lockdown holds,
`--quit` round-trips) and `--exit-probe` once per physical keyboard — the
only test that proves a keyboard can form and sustain the chord; synthetic
input bypasses the key matrix. Incident #2 proof of necessity: the original
4-key chord passed every synthetic layer and failed the physical press
(membrane rollover), which is why the chord is now Ctrl+Shift+F12 held 2s.

The process exits deliberately only when: the fault screen itself cannot be
created (exit **70** — WebView2 environment unusable) or a fatal startup
error occurs. An external supervisor should relaunch on those exits: Task
Scheduler ("At log on", restart on failure, station user) or NSSM. Kiosk
user: auto-logon, explorer replaced or taskbar locked down (the window is
fullscreen always-on-top with close blocked, but OS posture is the wrapper's
job).

## Per-SKU GPU check (TAURI.md)

Once per hardware SKU: run `--windowed` (devtools available in debug
builds), open `about://gpu` in the webview, confirm hardware-accelerated
rasterization + compositing. SwiftShader fallback ⇒ revisit the animation
budget before deploying that SKU. Trace the flash path once per SKU.

## Memory soak (ENGINEERING_NOTES doctrine)

Run INSIDE the packaged shell, not a browser: ~10× shift volume
(≈36,000 taps) at 150ms cadence via the sim tap path, sample
JSHeapUsedSize / JSEventListeners / DOM node count every 30s, assert
post-warmup flatness (≤1.15× median, no positive slope), include disconnect
blips and end-shift→new-shift cycles. One uncompressed 8h wall-clock run
before first factory deployment.

## Misc

- **USB power management:** disable USB selective suspend on the floor
  image — an idle-suspended reader appears absent (line pauses safely, but
  needlessly). Untested over long idle on reference hardware; recorded.
- **One reader per station:** the shell warns if more than one reader
  matches its filter and will accept taps from all of them — the floor
  image/config must guarantee exactly one.
- **Audio:** UI-owned, tuned relative; absolute SPL is deployment work —
  small powered speaker at the operator position, ~10–15 dB above ambient,
  calibrated with the UI's `?demo=1` loop (run WITHOUT the shell for that).
- **Logs:** `%APPDATA%\com.chainw3ar.station\logs\` roll daily and are not
  pruned yet — add retention before long deployments. `failures.jsonl` is
  append-only and feeds the future supervisor module; never prune it
  automatically.
- **Localization:** inject per-key overrides via `station.toml` `[strings]`
  (they ride the boot payload into `__STATION_STRINGS__`); set display-cased
  values only, and test with a pseudo-locale (~1.5× width headroom exists).
- **Time:** the shell stamps records with local system time; NTP posture of
  offline floor machines is an open deployment question.
