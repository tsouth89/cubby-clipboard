param(
    # Built cubby.exe to measure startup, idle CPU, and idle memory against.
    # Omit to measure only the in-process budgets.
    [string]$AppPath = "",
    # Seconds to sample idle CPU over. Shorter samples are dominated by noise.
    [int]$IdleSeconds = 20,
    # Seconds to wait after launch before sampling anything. The index build
    # runs at startup and is not idle work.
    [int]$SettleSeconds = 30,
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

# Every descendant process id, depth-first.
#
# Recursive on purpose. WebView2 puts its renderer, GPU, utility and crashpad
# processes under the WebView host, which is itself a child of the app -- so a
# direct-children-only walk misses six of Cubby's nine processes and under-reports
# its memory by more than half.
function Get-DescendantIds([int]$RootId) {
    $ids = @()
    $children = Get-CimInstance Win32_Process -Filter "ParentProcessId=$RootId" -ErrorAction SilentlyContinue
    foreach ($child in $children) {
        $ids += $child.ProcessId
        $ids += Get-DescendantIds -RootId $child.ProcessId
    }
    return $ids
}

# A process and every descendant of it. Cubby is a WebView app, so the whole
# tree is what the user sees in Task Manager -- but only *its* tree, resolved by
# parentage rather than by process name.
function Get-ProcessTree([int]$RootId) {
    $root = Get-Process -Id $RootId -ErrorAction SilentlyContinue
    if (-not $root) { return @() }
    $descendants = @(Get-DescendantIds -RootId $RootId) |
        Sort-Object -Unique |
        ForEach-Object { Get-Process -Id $_ -ErrorAction SilentlyContinue }
    return @($root) + @($descendants)
}

# CPU seconds per process id and total working set for a tree, read once.
#
# Every read is guarded: a process can exit between being enumerated and being
# measured, and reading TotalProcessorTime on an exited process throws rather
# than returning what it used.
function Get-TreeSample([int]$RootId) {
    $cpu = @{}
    $workingSet = 0
    foreach ($process in Get-ProcessTree -RootId $RootId) {
        if (-not $process) { continue }
        try {
            $cpu[$process.Id] = $process.TotalProcessorTime.TotalSeconds
            $workingSet += $process.WorkingSet64
        }
        catch {
            continue
        }
    }
    return [pscustomobject]@{ Cpu = $cpu; WorkingSet = $workingSet; Count = $cpu.Count }
}

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

    $process.Refresh()
    if (-not $process.HasExited) {
        # "Idle" has to mean idle. Sampling immediately after launch measures
        # the search index being built over the whole history, which on a real
        # 3,700-clip database reported 3.67% against a 1% budget when the
        # settled figure is 0.00%.
        Write-Host "  settling for $SettleSeconds s before sampling..."
        Start-Sleep -Seconds $SettleSeconds

        # An app that died during settling would otherwise be measured as an
        # empty tree and reported as 0.00% and 0.0 MiB -- two budgets passing
        # perfectly because nothing was running.
        $process.Refresh()
        if ($process.HasExited) {
            Add-Result "idle_cpu" "-" "not measured: the app exited during settling"
            Add-Result "idle_memory" "-" "not measured: the app exited during settling"
            return
        }

        # The tree is enumerated at both ends of the window rather than once.
        # WebView2 starts and retires renderer and utility processes while the
        # app sits there, so a snapshot taken before the sample misses whatever
        # appears during it, and reading a process that has since exited throws.
        $before = Get-TreeSample -RootId $process.Id
        $sampleStart = Get-Date
        Start-Sleep -Seconds $IdleSeconds

        $process.Refresh()
        if ($process.HasExited) {
            Add-Result "idle_cpu" "-" "not measured: the app exited while sampling"
            Add-Result "idle_memory" "-" "not measured: the app exited while sampling"
            return
        }

        $after = Get-TreeSample -RootId $process.Id
        if ($after.Count -eq 0) {
            Add-Result "idle_cpu" "-" "not measured: no live process to sample"
            Add-Result "idle_memory" "-" "not measured: no live process to sample"
            return
        }

        # Per process id, so one that appeared mid-window counts from zero
        # rather than making the total negative. CPU spent by a process that
        # exited during the window is lost; on an idle app that is a rounding
        # error, and over-reporting would be worse than under-reporting here.
        $cpuDelta = 0.0
        foreach ($id in $after.Cpu.Keys) {
            $prior = 0.0
            if ($before.Cpu.ContainsKey($id)) { $prior = $before.Cpu[$id] }
            $cpuDelta += ($after.Cpu[$id] - $prior)
        }
        $elapsed = ((Get-Date) - $sampleStart).TotalSeconds
        Add-Result "idle_cpu" ("{0:N2}%" -f (($cpuDelta / $elapsed) * 100)) "reported"

        # Measured at the end of the window, from the same enumeration.
        Add-Result "idle_memory" ("{0:N1} MiB" -f ($after.WorkingSet / 1MB)) "reported ($($after.Count) process tree)"

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
