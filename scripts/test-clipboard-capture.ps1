param(
    [int]$Runs = 3,
    [int]$BurstCount = 100,
    [int]$IntervalMs = 10,
    [int]$ContentionMs = 40,
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"
$repoRoot = Split-Path -Parent $PSScriptRoot
$tauriRoot = Join-Path $repoRoot "src-tauri"
$probePath = Join-Path $tauriRoot "target\debug\clipboard_probe.exe"

if (-not $SkipBuild) {
    Push-Location $tauriRoot
    try {
        cargo build --bin clipboard_probe
        if ($LASTEXITCODE -ne 0) {
            throw "clipboard_probe build failed"
        }
    }
    finally {
        Pop-Location
    }
}

if (-not (Test-Path -LiteralPath $probePath)) {
    throw "Clipboard probe not found at $probePath"
}

Write-Warning "This test intentionally replaces the current Windows clipboard contents."

$results = foreach ($run in 1..$Runs) {
    foreach ($scenario in @(
        @{ Name = "rapid"; ContentionMs = 0 },
        @{ Name = "contention"; ContentionMs = $ContentionMs }
    )) {
        $timeoutSeconds = [Math]::Max(
            30,
            [Math]::Ceiling(($BurstCount * ($IntervalMs + $scenario.ContentionMs)) / 1000) + 15
        )
        $output = & $probePath `
            --burst $BurstCount `
            --interval-ms $IntervalMs `
            --contention-ms $scenario.ContentionMs `
            --timeout-seconds $timeoutSeconds

        if ($LASTEXITCODE -ne 0) {
            throw "Clipboard capture run $run ($($scenario.Name)) failed"
        }

        $summary = $output |
            ForEach-Object { $_ | ConvertFrom-Json } |
            Where-Object event -eq "summary" |
            Select-Object -Last 1

        if (-not $summary -or -not $summary.passed) {
            throw "Clipboard capture run $run ($($scenario.Name)) did not produce a passing summary"
        }

        [pscustomobject]@{
            Run = $run
            Scenario = $scenario.Name
            Expected = $summary.expected_markers
            Observed = $summary.observed_markers
            Events = $summary.events
            ReadFailures = $summary.read_failures
            Passed = $summary.passed
        }
    }
}

$results | Format-Table -AutoSize

$failures = @($results | Where-Object { -not $_.Passed })
$summary = [pscustomobject]@{
    Passed = $failures.Count -eq 0
    Runs = $Runs
    Scenarios = $results.Count
    ClipboardUpdates = ($results | Measure-Object Observed -Sum).Sum
    ReadFailures = ($results | Measure-Object ReadFailures -Sum).Sum
    Failures = $failures.Count
}

$summary | Format-List
if (-not $summary.Passed) {
    exit 1
}
