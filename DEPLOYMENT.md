# DEPLOYMENT.md — deferred deployment work, recorded so it isn't rediscovered

None of this blocks the simulated-keying milestone; all of it is required
before a factory floor deployment.

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
