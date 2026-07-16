param(
    [int]$Runs = 10,
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"
$repoRoot = Split-Path -Parent $PSScriptRoot
$tauriRoot = Join-Path $repoRoot "src-tauri"
$harnessPath = Join-Path $tauriRoot "target\debug\paste_compat.exe"

if (-not $SkipBuild) {
    Push-Location $tauriRoot
    try {
        cargo build --bin paste_compat
        if ($LASTEXITCODE -ne 0) {
            throw "paste_compat build failed"
        }
    }
    finally {
        Pop-Location
    }
}

if (-not (Test-Path -LiteralPath $harnessPath)) {
    throw "Paste compatibility harness not found at $harnessPath"
}

$results = 1..$Runs | ForEach-Object {
    $output = & $harnessPath
    if ($LASTEXITCODE -ne 0) {
        throw "Paste compatibility run $_ failed"
    }
    $output | ConvertFrom-Json
}

$failures = @($results | Where-Object { -not $_.passed })
$summary = [pscustomobject]@{
    Passed = $failures.Count -eq 0
    Runs = $results.Count
    PasteCases = ($results | Measure-Object paste_cases -Sum).Sum
    InvalidTargetChecks = @($results | Where-Object invalid_target_safe).Count
    Failures = $failures.Count
}

$summary | Format-List
if (-not $summary.Passed) {
    exit 1
}
