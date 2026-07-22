param(
    [ValidateRange(1, 100)]
    [int]$Runs = 3,
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"
$repoRoot = Split-Path -Parent $PSScriptRoot
$tauriRoot = Join-Path $repoRoot "src-tauri"
$probePath = Join-Path $tauriRoot "target\debug\clipboard_probe.exe"

if (-not $SkipBuild) {
    Push-Location $tauriRoot
    try {
        cargo build --locked --bin clipboard_probe
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
    $output = & $probePath --fixtures --timeout-seconds 15
    $exitCode = $LASTEXITCODE

    $summary = $output |
        ForEach-Object { $_ | ConvertFrom-Json } |
        Where-Object event -eq "summary" |
        Select-Object -Last 1

    if ($exitCode -ne 0) {
        $detail = if ($summary) {
            $summary | ConvertTo-Json -Compress
        }
        else {
            $output -join [Environment]::NewLine
        }
        throw "Clipboard format fixture run $run failed: $detail"
    }

    if (-not $summary -or -not $summary.passed) {
        throw "Clipboard format fixture run $run did not produce a passing summary"
    }

    [pscustomobject]@{
        Run = $run
        Expected = $summary.expected_fixtures
        Observed = $summary.observed_fixtures
        PassedFixtures = $summary.passed_fixtures
        ReadFailures = $summary.read_failures
        Passed = $summary.passed
    }
}

$results | Format-Table -AutoSize

$failures = @($results | Where-Object { -not $_.Passed })
$summary = [pscustomobject]@{
    Passed = $failures.Count -eq 0
    Runs = $results.Count
    FixtureChecks = ($results | Measure-Object PassedFixtures -Sum).Sum
    ReadFailures = ($results | Measure-Object ReadFailures -Sum).Sum
    Failures = $failures.Count
}

$summary | Format-List
if (-not $summary.Passed) {
    exit 1
}
