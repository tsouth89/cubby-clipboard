param(
    [int]$TimeoutSeconds = 8,
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"
$repoRoot = Split-Path -Parent $PSScriptRoot
$tauriRoot = Join-Path $repoRoot "src-tauri"
$helperPath = Join-Path $tauriRoot "target\debug\win_v_helper.exe"

if (-not $SkipBuild) {
    Push-Location $tauriRoot
    try {
        cargo build --bin win_v_helper
        if ($LASTEXITCODE -ne 0) {
            throw "win_v_helper build failed"
        }
    }
    finally {
        Pop-Location
    }
}

if (-not (Test-Path -LiteralPath $helperPath)) {
    throw "Helper binary not found at $helperPath"
}

$nativeSource = @'
using System;
using System.Runtime.InteropServices;

public static class CubbyShortcutTestInput
{
    [DllImport("user32.dll")]
    public static extern void keybd_event(byte virtualKey, byte scanCode, uint flags, UIntPtr extraInfo);

    [DllImport("user32.dll")]
    public static extern short GetAsyncKeyState(int virtualKey);
}
'@

if (-not ("CubbyShortcutTestInput" -as [type])) {
    Add-Type -TypeDefinition $nativeSource
}

$keyUp = 0x0002
$testMarker = [UIntPtr]0x43554254
$keys = @{
    Ctrl = 0xA2
    Shift = 0xA0
    Win = 0x5B
    V = 0x56
}

function Send-KeyDown([byte]$key) {
    [CubbyShortcutTestInput]::keybd_event($key, 0, 0, $testMarker)
    Start-Sleep -Milliseconds 35
}

function Send-KeyUp([byte]$key) {
    [CubbyShortcutTestInput]::keybd_event($key, 0, $keyUp, $testMarker)
    Start-Sleep -Milliseconds 35
}

function Send-KeyTap([byte]$key) {
    Send-KeyDown $key
    Send-KeyUp $key
}

function Send-WinV {
    Send-KeyDown $keys.Win
    Send-KeyTap $keys.V
    Send-KeyUp $keys.Win
}

function Release-TestKeys {
    foreach ($key in @($keys.V, $keys.Shift, $keys.Ctrl, $keys.Win, 0x5C, 0xA1, 0xA3, 0xA4, 0xA5)) {
        [CubbyShortcutTestInput]::keybd_event([byte]$key, 0, $keyUp, $testMarker)
    }
}

$stamp = Get-Date -Format "yyyyMMdd-HHmmss-fff"
$stdoutPath = Join-Path $env:TEMP "cubby-win-v-test-$stamp.out.log"
$stderrPath = Join-Path $env:TEMP "cubby-win-v-test-$stamp.err.log"
$process = $null

try {
    Release-TestKeys
    $process = Start-Process `
        -FilePath $helperPath `
        -ArgumentList @("--timeout-seconds", $TimeoutSeconds, "--accept-injected-test-events") `
        -WindowStyle Hidden `
        -RedirectStandardOutput $stdoutPath `
        -RedirectStandardError $stderrPath `
        -PassThru

    $deadline = (Get-Date).AddSeconds(3)
    do {
        Start-Sleep -Milliseconds 50
        $output = if (Test-Path -LiteralPath $stdoutPath) {
            Get-Content -LiteralPath $stdoutPath -Raw
        } else {
            ""
        }
    } while ($output -notmatch '"event":"ready"' -and (Get-Date) -lt $deadline)

    if ($output -notmatch '"event":"ready"') {
        throw "Helper did not become ready. stderr: $(Get-Content -LiteralPath $stderrPath -Raw)"
    }

    # Plain V must pass through without activating Cubby.
    Send-KeyTap $keys.V

    # Two complete chords must each activate.
    Send-WinV
    Send-WinV

    # Holding Win and tapping V twice must activate twice. Pressing Shift next
    # exercises restoration of the still-physical Windows key.
    Send-KeyDown $keys.Win
    Send-KeyTap $keys.V
    Send-KeyTap $keys.V
    Send-KeyTap $keys.Shift
    Send-KeyUp $keys.Win

    # Extra modifiers make this a non-exact chord and must not activate Cubby.
    Send-KeyDown $keys.Ctrl
    Send-WinV
    Send-KeyUp $keys.Ctrl

    $process.WaitForExit(($TimeoutSeconds + 3) * 1000) | Out-Null
    if (-not $process.HasExited) {
        throw "Helper did not stop after its timeout"
    }

    $events = Get-Content -LiteralPath $stdoutPath |
        Where-Object { $_.Trim() } |
        ForEach-Object { $_ | ConvertFrom-Json }

    $activationCount = @($events | Where-Object event -eq "win_v").Count
    $restoreCount = @($events | Where-Object event -eq "win_restored").Count
    $failures = @($events | Where-Object event -in @("error", "injection_failed", "restore_failed"))

    if ($activationCount -ne 4) {
        throw "Expected 4 Win+V activations, observed $activationCount"
    }
    if ($restoreCount -ne 1) {
        throw "Expected 1 Windows-key restoration, observed $restoreCount"
    }
    if ($failures.Count -ne 0) {
        throw "Helper reported failures: $($failures | ConvertTo-Json -Compress)"
    }

    Release-TestKeys
    Start-Sleep -Milliseconds 100
    $keysStillDown = @(
        $keys.Values + @(0x5C, 0xA1, 0xA3, 0xA4, 0xA5) |
            Sort-Object -Unique |
            Where-Object { [CubbyShortcutTestInput]::GetAsyncKeyState($_) -lt 0 }
    )
    if ($keysStillDown.Count -ne 0) {
        throw "Test keys remain logically down: $($keysStillDown -join ', ')"
    }

    [pscustomobject]@{
        Passed = $true
        Activations = $activationCount
        WinRestorations = $restoreCount
        KeysStillDown = 0
        Log = $stdoutPath
    } | Format-List
}
finally {
    Release-TestKeys
    if ($process -and -not $process.HasExited) {
        Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
    }
}
