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

public static class CubbyRemoteTriggerTestInput
{
    [DllImport("user32.dll")]
    public static extern void keybd_event(byte virtualKey, byte scanCode, uint flags, UIntPtr extraInfo);

    [DllImport("user32.dll")]
    public static extern short GetAsyncKeyState(int virtualKey);
}
'@

if (-not ("CubbyRemoteTriggerTestInput" -as [type])) {
    Add-Type -TypeDefinition $nativeSource
}

$leftCtrl = [byte]0xA2
$rightCtrl = [byte]0xA3
$keyUp = 0x0002
$testMarker = [UIntPtr]0x43554254
$listener = [System.Net.Sockets.UdpClient]::new(0)
$listener.Client.ReceiveTimeout = 3000
$activationPort = ([System.Net.IPEndPoint]$listener.Client.LocalEndPoint).Port
$stamp = Get-Date -Format "yyyyMMdd-HHmmss-fff"
$stdoutPath = Join-Path $env:TEMP "cubby-remote-trigger-test-$stamp.out.log"
$stderrPath = Join-Path $env:TEMP "cubby-remote-trigger-test-$stamp.err.log"
$process = $null

function Send-CtrlTap([byte]$key) {
    [CubbyRemoteTriggerTestInput]::keybd_event($key, 0, 0, $testMarker)
    Start-Sleep -Milliseconds 35
    [CubbyRemoteTriggerTestInput]::keybd_event($key, 0, $keyUp, $testMarker)
    Start-Sleep -Milliseconds 100
}

function Release-CtrlKeys {
    [CubbyRemoteTriggerTestInput]::keybd_event($leftCtrl, 0, $keyUp, $testMarker)
    [CubbyRemoteTriggerTestInput]::keybd_event($rightCtrl, 0, $keyUp, $testMarker)
}

try {
    Release-CtrlKeys
    $process = Start-Process `
        -FilePath $helperPath `
        -ArgumentList @(
            "--timeout-seconds", $TimeoutSeconds,
            "--accept-injected-test-events",
            "--activation-port", $activationPort
        ) `
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

    Send-CtrlTap $leftCtrl
    Send-CtrlTap $leftCtrl

    $remoteEndpoint = [System.Net.IPEndPoint]::new([System.Net.IPAddress]::Any, 0)
    $message = $listener.Receive([ref]$remoteEndpoint)
    $messageText = [System.Text.Encoding]::UTF8.GetString($message)
    if ($messageText -ne "activate") {
        throw "Expected direct activation message, received '$messageText'"
    }

    Start-Sleep -Milliseconds 150
    $events = Get-Content -LiteralPath $stdoutPath |
        Where-Object { $_.Trim() } |
        ForEach-Object { $_ | ConvertFrom-Json }
    $triggerCount = @($events | Where-Object event -eq "remote_trigger").Count
    $failures = @($events | Where-Object event -in @("error", "activation_failed"))

    if ($triggerCount -ne 1) {
        throw "Expected 1 remote trigger activation, observed $triggerCount"
    }
    if ($failures.Count -ne 0) {
        throw "Helper reported failures: $($failures | ConvertTo-Json -Compress)"
    }

    Release-CtrlKeys
    Start-Sleep -Milliseconds 100
    if (
        [CubbyRemoteTriggerTestInput]::GetAsyncKeyState($leftCtrl) -lt 0 -or
        [CubbyRemoteTriggerTestInput]::GetAsyncKeyState($rightCtrl) -lt 0
    ) {
        throw "A Ctrl key remains logically down"
    }

    [pscustomobject]@{
        Passed = $true
        Trigger = "Double-tap Left Ctrl"
        DirectActivations = $triggerCount
        KeysStillDown = 0
        Log = $stdoutPath
    } | Format-List
}
finally {
    Release-CtrlKeys
    $listener.Dispose()
    if ($process -and -not $process.HasExited) {
        Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
    }
}
