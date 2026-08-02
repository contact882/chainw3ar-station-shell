# exit-gate.ps1 -- automated escape-path regression gate (E4, incident #2
# 2026-08-01). Proves every documented dev exit WORKS and the production
# lockdown HOLDS, against the real built exe on this machine. The README's
# escape table cites this gate's output; a README row without a gate case is
# UNVERIFIED by definition.
#
# Needs an interactive desktop session (injects synthetic keyboard input),
# like --reader-probe and the conformance gate. Two cases take the screen
# fullscreen for a few seconds each.
#
# Usage:  powershell -File scripts\exit-gate.ps1 [-Exe path\to\station-shell.exe]
# Exit:   0 = all cases pass, 1 = any failure.

param(
    [string]$Exe = (Join-Path $PSScriptRoot "..\src-tauri\target\release\station-shell.exe")
)

$Exe = (Resolve-Path $Exe).Path
# The shell's daily log rolls on the UTC date, not local (bit this gate once
# at 21:xx local / 01:xx UTC — behaviors passed, every log assertion missed).
$LogFile = Join-Path $env:APPDATA "com.chainw3ar.station\logs\shell.log.$([DateTime]::UtcNow.ToString('yyyy-MM-dd'))"
$Fails = 0

# Chord since incident #2's hardware finding: Ctrl+Shift+F12 HELD 2s.
# (The 4-key Ctrl+Alt+Shift+Q failed on the reference membrane keyboard.)
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public class ExitGateNative {
    [DllImport("user32.dll")] static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
    [DllImport("user32.dll", SetLastError=true)] public static extern bool RegisterHotKey(IntPtr h, int id, uint mods, uint vk);
    [DllImport("user32.dll")] public static extern bool UnregisterHotKey(IntPtr h, int id);
    // Ctrl=0x11 Shift=0x10 F12=0x7B
    static void Down() { keybd_event(0x11,0,0,UIntPtr.Zero); keybd_event(0x10,0,0,UIntPtr.Zero); keybd_event(0x7B,0,0,UIntPtr.Zero); }
    static void Up()   { keybd_event(0x7B,0,2,UIntPtr.Zero); keybd_event(0x10,0,2,UIntPtr.Zero); keybd_event(0x11,0,2,UIntPtr.Zero); }
    // Short tap: must NEVER exit the shell (sub-hold accidental press).
    public static void TapChord() { Down(); System.Threading.Thread.Sleep(300); Up(); }
    // Sustained hold past the 2s requirement: must exit an armed shell.
    public static void HoldChord() { Down(); System.Threading.Thread.Sleep(2600); Up(); }
}
'@

function Get-LogLen {
    if (Test-Path $LogFile) { (Get-Item $LogFile).Length } else { 0 }
}

# Read the log from a byte offset (the file is held open by running shells).
function Read-NewLog([long]$From) {
    if (-not (Test-Path $LogFile)) { return "" }
    $fs = [System.IO.File]::Open($LogFile, 'Open', 'Read', 'ReadWrite')
    try {
        if ($From -ge $fs.Length) { return "" }
        $fs.Position = $From
        $sr = New-Object System.IO.StreamReader($fs, [System.Text.Encoding]::UTF8)
        return $sr.ReadToEnd()
    } finally { $fs.Dispose() }
}

function Wait-Pattern([string]$Pattern, [long]$From, [int]$TimeoutSec = 10) {
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    while ($sw.Elapsed.TotalSeconds -lt $TimeoutSec) {
        if ((Read-NewLog $From) -match [regex]::Escape($Pattern)) { return $true }
        Start-Sleep -Milliseconds 200
    }
    return $false
}

function Assert([bool]$Cond, [string]$What) {
    if ($Cond) { Write-Host "  ok    $What" }
    else { Write-Host "  FAIL  $What" -ForegroundColor Red; $script:Fails++ }
}

function Stop-Leftovers {
    Get-Process station-shell -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 500
}

Write-Host "EXIT GATE against: $Exe"
if (Get-Process station-shell -ErrorAction SilentlyContinue) {
    Write-Host "station-shell is already running -- refusing to gate against a live station." -ForegroundColor Red
    exit 1
}

# ---- Case 0: a short TAP must NOT exit (accidental-press pin) --------------
Write-Host "case: short tap does NOT exit (hold requirement)"
$mark = Get-LogLen
$p = Start-Process -FilePath $Exe -ArgumentList @("--windowed", "--dev-exit") -PassThru
Assert (Wait-Pattern "dev exit chord armed (delivery verified)" $mark 15) "canary-verified arming logged"
[ExitGateNative]::TapChord()
Start-Sleep -Seconds 4
Assert (-not $p.HasExited) "still running after sub-hold tap"
[ExitGateNative]::HoldChord()
Assert ($p.WaitForExit(8000)) "then exits on a sustained hold"
if (-not $p.HasExited) { Stop-Leftovers }

# ---- Cases 1-4: armed launches must exit on the held chord, all postures ----
$ArmedCases = @(
    @{ Name = "windowed --dev-exit --sim"; Args = @("--windowed", "--dev-exit", "--sim") },
    @{ Name = "windowed --dev-exit";       Args = @("--windowed", "--dev-exit") },
    @{ Name = "KIOSK    --dev-exit --sim"; Args = @("--dev-exit", "--sim") },
    @{ Name = "KIOSK    --dev-exit";       Args = @("--dev-exit") }
)
foreach ($case in $ArmedCases) {
    Write-Host "case: held chord exits [$($case.Name)]"
    $mark = Get-LogLen
    $p = Start-Process -FilePath $Exe -ArgumentList $case.Args -PassThru
    Assert (Wait-Pattern "dev exit chord armed (delivery verified)" $mark 15) "canary-verified arming logged"
    [ExitGateNative]::HoldChord()
    Assert ($p.WaitForExit(8000)) "process exited on held chord"
    if (-not $p.HasExited) { Stop-Leftovers }
    else { Assert ($p.ExitCode -eq 0) "exit code 0 (got $($p.ExitCode))" }
    Assert ((Read-NewLog $mark) -match "shutting down cleanly") "clean-shutdown line logged"
}

# ---- Case 5: --quit ends an armed instance ---------------------------------
Write-Host "case: --quit exits an armed instance"
$mark = Get-LogLen
$p = Start-Process -FilePath $Exe -ArgumentList @("--windowed", "--dev-exit") -PassThru
Assert (Wait-Pattern "dev exit chord armed (delivery verified)" $mark 15) "canary-verified arming logged"
$sender = Start-Process -FilePath $Exe -ArgumentList @("--quit") -PassThru -Wait
Assert ($sender.ExitCode -eq 0) "sender delivered (exit 0, got $($sender.ExitCode))"
Assert ($p.WaitForExit(8000)) "armed instance exited on --quit"
Assert ((Read-NewLog $mark) -match "administrative quit") "administrative-quit line logged"
if (-not $p.HasExited) { Stop-Leftovers }

# ---- Case 6: bare kiosk lockdown HOLDS (negative proof) --------------------
Write-Host "case: bare kiosk ignores chord AND refuses --quit (lockdown)"
$mark = Get-LogLen
$p = Start-Process -FilePath $Exe -PassThru
Assert (Wait-Pattern "kiosk launch without exit chord" $mark 15) "no-local-escape WARNING logged"
Start-Sleep -Seconds 2
[ExitGateNative]::HoldChord()
Start-Sleep -Seconds 3
Assert (-not $p.HasExited) "still running after injected held chord"
$sender = Start-Process -FilePath $Exe -ArgumentList @("--quit") -PassThru -Wait
Assert ($sender.ExitCode -eq 0) "quit sender delivered (exit 0)"
Assert (Wait-Pattern "quit refused" $mark 8) "refusal logged (lockdown posture)"
Start-Sleep -Seconds 1
Assert (-not $p.HasExited) "still running after refused --quit"
Stop-Leftovers
Assert $true "(cleaned up via taskkill-equivalent -- the only intended path)"

# ---- Case 7: --quit with nothing running anywhere --> exit 3 ---------------
Write-Host "case: --quit with no station-shell process anywhere"
$sender = Start-Process -FilePath $Exe -ArgumentList @("--quit") -PassThru -Wait
Assert ($sender.ExitCode -eq 3) "exit 3 = no station anywhere (got $($sender.ExitCode))"

# ---- Case 7b: --quit with an UNREACHABLE name-match --> exit 4 --------------
# An --exit-probe instance skips single-instance registration by design, so
# it is a real station-shell.exe process the sender cannot reach — exactly
# exit 4's honest semantics (name match only). This produces the SAME-SESSION
# unreachable variant; the cross-session variant (session-0 agent, other-user
# RDP) is source-proven but UNVERIFIED here — it needs an elevated second
# session this gate does not have. Do not upgrade it on this case's strength.
Write-Host "case: --quit with unreachable station-shell process -> exit 4"
$mark = Get-LogLen
$probe = Start-Process -FilePath $Exe -ArgumentList @("--exit-probe") -PassThru
Start-Sleep -Seconds 2
Assert (-not $probe.HasExited) "probe instance (unreachable name-match) is running"
$sender = Start-Process -FilePath $Exe -ArgumentList @("--quit") -PassThru -Wait
Assert ($sender.ExitCode -eq 4) "exit 4 = unreachable process exists (got $($sender.ExitCode))"
Assert (Wait-Pattern "name match only (exit 4)" $mark 8) "honest-semantics line logged"
Assert (-not $probe.HasExited) "probe instance untouched by the sender"
Stop-Leftovers

# ---- Case 8: a held chord makes an armed launch REFUSE (never silent) ------
Write-Host "case: chord held by another process -> armed launch aborts"
$mark = Get-LogLen
# MOD_CONTROL(0x2)|MOD_SHIFT(0x4) + VK_F12(0x7B)
$held = [ExitGateNative]::RegisterHotKey([IntPtr]::Zero, 0x0C0E, 0x6, 0x7B)
Assert $held "gate holds Ctrl+Shift+F12"
$p = Start-Process -FilePath $Exe -ArgumentList @("--windowed", "--dev-exit") -PassThru
$exited = $p.WaitForExit(15000)
Assert $exited "launch aborted instead of running unarmed"
if ($exited) { Assert ($p.ExitCode -ne 0) "nonzero exit (got $($p.ExitCode))" }
Assert (-not ((Read-NewLog $mark) -match "dev exit chord armed")) "no false 'armed' line"
[ExitGateNative]::UnregisterHotKey([IntPtr]::Zero, 0x0C0E) | Out-Null
if (-not $exited) { Stop-Leftovers }

Write-Host ""
if ($Fails -eq 0) { Write-Host "EXIT GATE: PASS (all cases)" -ForegroundColor Green; exit 0 }
else { Write-Host "EXIT GATE: FAIL ($Fails assertion(s))" -ForegroundColor Red; exit 1 }
