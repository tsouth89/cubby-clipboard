param(
    # Comma-separated app classes to run. Omit for all of them.
    # Use -List to see the available names.
    [string]$Only = "",
    [switch]$List,
    [switch]$KeepApps,
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"
$repoRoot = Split-Path -Parent $PSScriptRoot
$tauriRoot = Join-Path $repoRoot "src-tauri"
$matrixPath = Join-Path $tauriRoot "target\debug\compat_matrix.exe"

if (-not $SkipBuild) {
    Push-Location $tauriRoot
    try {
        cargo build --locked --features dev-harness --bin compat_matrix
        if ($LASTEXITCODE -ne 0) {
            throw "compat_matrix build failed"
        }
    }
    finally {
        Pop-Location
    }
}

if (-not (Test-Path -LiteralPath $matrixPath)) {
    throw "Compatibility matrix not found at $matrixPath"
}

if ($List) {
    & $matrixPath --list
    return
}

Write-Warning "This test replaces the current Windows clipboard contents, and opens and closes real applications. Do not use the machine while it runs."

$matrixArgs = @()
if ($Only) { $matrixArgs += @("--only", $Only) }
if ($KeepApps) { $matrixArgs += "--keep-apps" }

$output = & $matrixPath @matrixArgs
$exitCode = $LASTEXITCODE

$events = $output | ForEach-Object { $_ | ConvertFrom-Json }
$rows = @($events | Where-Object event -eq "row")
$summary = $events | Where-Object event -eq "summary" | Select-Object -Last 1

if (-not $summary) {
    $output | Write-Host
    throw "Compatibility matrix did not produce a summary"
}

$rows |
    Select-Object @{ Name = "Class"; Expression = { $_.class } },
                  @{ Name = "App"; Expression = { $_.app } },
                  @{ Name = "Status"; Expression = { $_.status } },
                  @{ Name = "Detail"; Expression = { $_.detail } } |
    Format-Table -AutoSize -Wrap

[pscustomobject]@{
    Passed            = $summary.passed
    RowsPassed        = $summary.rows_passed
    RowsFailed        = $summary.rows_failed
    RowsSkipped       = $summary.rows_skipped
    Checks            = $summary.checks
    ClassesNotCovered = ($summary.classes_not_covered -join ", ")
} | Format-List

# Skipped rows do not fail the run: a machine without Word cannot prove anything
# about Word. They are printed instead, because a green run that skipped six
# classes is not the same result as a green run that covered them.
if ($summary.classes_not_covered.Count -gt 0) {
    Write-Warning "Classes with no executed row: $($summary.classes_not_covered -join ', '). This run proved nothing about them."
}

if (-not $summary.passed) {
    Write-Host "`nFailures:" -ForegroundColor Red
    foreach ($failure in $summary.failures) {
        Write-Host ("  {0} / {1} / {2} / {3}: {4}" -f $failure.class, $failure.app, $failure.format, $failure.step, $failure.detail)
    }
    exit 1
}

exit $exitCode
