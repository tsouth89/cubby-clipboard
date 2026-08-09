param(
    # Built cubby.exe to measure startup, idle CPU, and idle memory against.
    # Omit to measure only the in-process budgets.
    [string]$AppPath = "",
    # Seconds to sample idle CPU over. Shorter samples are dominated by startup.
    [int]$IdleSeconds = 20,
    [switch]$SkipInProcess
)

$ErrorActionPreference = "Stop"
$repoRoot = Split-Path -Parent $PSScriptRoot
$tauriRoot = Join-Path $repoRoot "src-tauri"

# Budgets that this script cannot measure on its own. Named rather than dropped:
# a report that silently omits them looks complete when it is not.
$notMeasuredHere = @{
    "shortcut_to_visible" = "needs a driven global hotkey against a running app"
    "paste_completion"    = "covered by scripts/test-paste-compat.ps1"
}

$results = New-Object System.Collections.Generic.List[object]

function Add-Result([string]$Id, [string]$Value, [string]$Status) {
    $results.Add([pscustomobject]@{ Budget = $Id; Measured = $Value; Status = $Status })
}

if (-not $SkipInProcess) {
    Write-Host "Measuring in-process budgets (single-threaded; this builds and runs the ignored perf tests)..." -ForegroundColor Cyan
    Push-Location $tauriRoot
    try {
        # --test-threads=1 matters: the reported measurements include an
        # allocation-sensitive one, and parallel tests would pollute it.
        $output = cargo test --locked --lib perf_budget -- --include-ignored --test-threads=1 --nocapture 2>&1
        $output | Where-Object { $_ -match "^\s*\w+\s+.*/\s" } | ForEach-Object { Write-Host "  $_" }
        if ($LASTEXITCODE -ne 0) {
            $output | Write-Host
            throw "In-process perf budget measurements failed"
        }
    }
    finally {
        Pop-Location
    }
}

if ($AppPath) {
    if (-not (Test-Path -LiteralPath $AppPath)) {
        throw "App not found at $AppPath"
    }

    Write-Host "`nMeasuring whole-process budgets against $AppPath..." -ForegroundColor Cyan
    $startedAt = Get-Date
    $process = Start-Process -FilePath $AppPath -PassThru

    # process_startup: process start to a visible main window.
    $visibleAt = $null
    while (((Get-Date) - $startedAt).TotalSeconds -lt 30) {
        $process.Refresh()
        if ($process.HasExited) { break }
        if ($process.MainWindowHandle -ne 0) { $visibleAt = Get-Date; break }
        Start-Sleep -Milliseconds 25
    }

    if ($visibleAt) {
        Add-Result "process_startup" ("{0:N0} ms" -f ($visibleAt - $startedAt).TotalMilliseconds) "reported"
    }
    else {
        # Cubby starts to the tray with no window until the hotkey is pressed,
        # so this is expected rather than a failure. Say which it is.
        Add-Result "process_startup" "no main window within 30 s" "not measured (starts to tray)"
    }

    # idle_cpu: CPU time consumed over the sample window, as a share of one core.
    $process.Refresh()
    if (-not $process.HasExited) {
        $cpuBefore = $process.TotalProcessorTime
        Start-Sleep -Seconds $IdleSeconds
        $process.Refresh()
        $cpuAfter = $process.TotalProcessorTime
        $cpuPercent = (($cpuAfter - $cpuBefore).TotalSeconds / $IdleSeconds) * 100
        Add-Result "idle_cpu" ("{0:N2}%" -f $cpuPercent) "reported"

        # idle_memory: working set of the whole process tree. Cubby is a WebView
        # app, so the renderer's memory is part of what the user sees.
        $tree = @($process) + @(Get-Process -ErrorAction SilentlyContinue |
            Where-Object { $_.ProcessName -like "*WebView*" -or $_.ProcessName -eq "msedgewebview2" })
        $workingSet = ($tree | Measure-Object WorkingSet64 -Sum).Sum
        Add-Result "idle_memory" ("{0:N1} MiB" -f ($workingSet / 1MB)) "reported (includes WebView processes)"

        $process.CloseMainWindow() | Out-Null
        Start-Sleep -Seconds 2
        if (-not $process.HasExited) { $process | Stop-Process -Force }
    }
    else {
        Add-Result "process_startup" "process exited during startup" "failed to launch"
    }
}
else {
    Write-Warning "No -AppPath given: process_startup, idle_cpu, and idle_memory were not measured."
}

foreach ($id in $notMeasuredHere.Keys) {
    Add-Result $id "-" ("not measured here: " + $notMeasuredHere[$id])
}

if ($results.Count -gt 0) {
    Write-Host ""
    $results | Format-Table -AutoSize -Wrap
}

Write-Host "Reported budgets are informational and never fail this script. Enforced budgets are asserted by 'cargo test'." -ForegroundColor DarkGray
