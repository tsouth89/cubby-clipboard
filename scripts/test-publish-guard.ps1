# Tests the rule that decides whether a published release object may be
# replaced.
#
# This guard is the only thing standing between a re-run and a live download URL
# changing under users, so it is worth a test that fails rather than a comment
# that reassures. Deliberately not a Pester suite: the runners and this machine
# disagree about which Pester major version is present, and the rule is a pure
# function that needs no framework to check.

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$script = Join-Path $PSScriptRoot "publish-store-installer.ps1"
if (-not (Test-Path -LiteralPath $script)) {
    throw "publish-store-installer.ps1 not found next to this test"
}

# Load the functions without running the script: dot-sourcing would demand the
# mandatory parameters and start talking to R2.
$ast = [System.Management.Automation.Language.Parser]::ParseFile($script, [ref]$null, [ref]$null)
$functions = $ast.FindAll(
    { param($node) $node -is [System.Management.Automation.Language.FunctionDefinitionAst] },
    $true
)
foreach ($function in $functions) {
    . ([scriptblock]::Create($function.Extent.Text))
}

$failures = New-Object System.Collections.Generic.List[string]

function Assert-Action {
    param([string]$What, [string]$Expected, [string]$Actual)
    if ($Expected -eq $Actual) {
        Write-Host "  ok   $What -> $Actual"
    }
    else {
        Write-Host "  FAIL $What -> expected '$Expected', got '$Actual'" -ForegroundColor Red
        $failures.Add($What)
    }
}

Write-Host "Resolve-ImmutableObjectAction"

# Identical bytes: nothing to do, and it must not matter whether the release is
# out. Re-uploading the same object is a no-op, not a violation.
Assert-Action "same bytes, published release" "skip" `
(Resolve-ImmutableObjectAction -BytesMatch $true -ReleaseIsDraft $false)
Assert-Action "same bytes, draft release" "skip" `
(Resolve-ImmutableObjectAction -BytesMatch $true -ReleaseIsDraft $true)

# Different bytes on a draft: the retry case this exists to allow. Nothing has
# shipped from the URL yet, so replacing it changes nothing a user can see.
Assert-Action "different bytes, draft release" "replace" `
(Resolve-ImmutableObjectAction -BytesMatch $false -ReleaseIsDraft $true)

# Different bytes on a published release: the case that must never be allowed.
# Signing is non-deterministic, so "different bytes" is the normal outcome of a
# rebuild -- if this ever returns anything but refuse, a re-run silently swaps
# the installer behind a live download link.
Assert-Action "different bytes, published release" "refuse" `
(Resolve-ImmutableObjectAction -BytesMatch $false -ReleaseIsDraft $false)

Write-Host "Test-ReleaseIsDraft"

# No tag means no evidence the release is unpublished, so the answer is false
# and the caller refuses. Failing closed matters more here than being helpful.
Assert-Action "empty tag is not a draft" "False" `
([string](Test-ReleaseIsDraft -Tag ""))
Assert-Action "whitespace tag is not a draft" "False" `
([string](Test-ReleaseIsDraft -Tag "   "))

if ($failures.Count -gt 0) {
    Write-Host ""
    throw "$($failures.Count) publish-guard assertion(s) failed: $($failures -join ', ')"
}

Write-Host ""
Write-Host "publish guard: all assertions passed"
