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
$LogFile = Join-Path $env:APPDATA "com.chainw3ar.station\logs\shell.log.$(Get-Date -Format yyyy-MM-dd)"
$Fails = 0

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public class ExitGateNative {
    [DllImport("user32.dll")] static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
    [DllImport("user32.dll", SetLastError=true)] public static extern bool RegisterHotKey(IntPtr h, int id, uint mods, uint vk);
    [DllImport("user32.dll")] public static extern bool UnregisterHotKey(IntPtr h, int id);
    public static void FireChord() {
        keybd_event(0x11,0,0,UIntPtr.Zero); keybd_event(0x12,0,0,UIntPtr.Zero); keybd_event(0x10,0,0,UIntPtr.Zero);
        keybd_event(0x51,0,0,UIntPtr.Zero); keybd_event(0x51,0,2,UIntPtr.Zero);
        keybd_event(0x10,0,2,UIntPtr.Zero); keybd_event(0x12,0,2,UIntPtr.Zero); keybd_event(0x11,0,2,UIntPtr.Zero);
    }
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

# ---- Cases 1-4: armed launches must exit on the chord, all postures --------
$ArmedCases = @(
    @{ Name = "windowed --dev-exit --sim"; Args = @("--windowed", "--dev-exit", "--sim") },
    @{ Name = "windowed --dev-exit";       Args = @("--windowed", "--dev-exit") },
    @{ Name = "KIOSK    --dev-exit --sim"; Args = @("--dev-exit", "--sim") },
    @{ Name = "KIOSK    --dev-exit";       Args = @("--dev-exit") }
)
foreach ($case in $ArmedCases) {
    Write-Host "case: chord exits [$($case.Name)]"
    $mark = Get-LogLen
    $p = Start-Process -FilePath $Exe -ArgumentList $case.Args -PassThru
    Assert (Wait-Pattern "dev exit chord armed (delivery verified)" $mark 15) "canary-verified arming logged"
    [ExitGateNative]::FireChord()
    Assert ($p.WaitForExit(8000)) "process exited on chord"
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
[ExitGateNative]::FireChord()
Start-Sleep -Seconds 3
Assert (-not $p.HasExited) "still running after injected chord"
$sender = Start-Process -FilePath $Exe -ArgumentList @("--quit") -PassThru -Wait
Assert ($sender.ExitCode -eq 0) "quit sender delivered (exit 0)"
Assert (Wait-Pattern "quit refused" $mark 8) "refusal logged (lockdown posture)"
Start-Sleep -Seconds 1
Assert (-not $p.HasExited) "still running after refused --quit"
Stop-Leftovers
Assert $true "(cleaned up via taskkill-equivalent -- the only intended path)"

# ---- Case 7: --quit with nothing running -----------------------------------
Write-Host "case: --quit with no instance running"
$sender = Start-Process -FilePath $Exe -ArgumentList @("--quit") -PassThru -Wait
Assert ($sender.ExitCode -eq 3) "exit 3 = nothing to quit (got $($sender.ExitCode))"

# ---- Case 8: a held chord makes an armed launch REFUSE (never silent) ------
Write-Host "case: chord held by another process -> armed launch aborts"
$mark = Get-LogLen
$held = [ExitGateNative]::RegisterHotKey([IntPtr]::Zero, 0x0C0E, 0x7, 0x51)
Assert $held "gate holds Ctrl+Alt+Shift+Q"
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
