<#
.SYNOPSIS
    Authenticode-signs a single file with Azure Trusted Signing.

.DESCRIPTION
    Invoked by Tauri's bundle.windows.signCommand for every executable it
    bundles, so the app binary and the NSIS uninstaller are signed *before*
    they are packed into the installer. Signing only the finished installer
    (which is what CI did previously) leaves an unsigned cubby.exe on disk,
    and an unsigned binary in %LOCALAPPDATA% that writes a Run key is what
    trips Defender's Behavior:Win32/Persistence.A!ml heuristic.

    Configuration comes from the environment so no tenant details live in the
    repo:

        ARTIFACT_SIGNING_ENDPOINT
        ARTIFACT_SIGNING_ACCOUNT_NAME
        ARTIFACT_SIGNING_CERTIFICATE_PROFILE_NAME

    Credentials are resolved by trusted-signing-cli through Azure's default
    credential chain, which picks up the OIDC login that azure/login performs
    in CI.

    When those variables are absent the script exits successfully without
    signing, so ordinary local builds keep working for contributors who have
    no access to the signing account. Set CUBBY_REQUIRE_SIGNING=1 (CI does) to
    turn a missing configuration into a hard failure instead -- otherwise a
    typo in a workflow variable would silently ship unsigned binaries again.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$Path
)

$ErrorActionPreference = "Stop"

$endpoint = $env:ARTIFACT_SIGNING_ENDPOINT
$account = $env:ARTIFACT_SIGNING_ACCOUNT_NAME
$profileName = $env:ARTIFACT_SIGNING_CERTIFICATE_PROFILE_NAME
$required = $env:CUBBY_REQUIRE_SIGNING -eq "1"

$missing = @()
if ([string]::IsNullOrWhiteSpace($endpoint)) { $missing += "ARTIFACT_SIGNING_ENDPOINT" }
if ([string]::IsNullOrWhiteSpace($account)) { $missing += "ARTIFACT_SIGNING_ACCOUNT_NAME" }
if ([string]::IsNullOrWhiteSpace($profileName)) { $missing += "ARTIFACT_SIGNING_CERTIFICATE_PROFILE_NAME" }

if ($missing.Count -gt 0) {
    $joined = $missing -join ", "
    if ($required) {
        throw "CUBBY_REQUIRE_SIGNING is set but the signing configuration is incomplete: $joined."
    }
    Write-Host "sign-windows: skipping $Path (unset: $joined)."
    exit 0
}

if (-not (Test-Path -LiteralPath $Path)) {
    throw "sign-windows: nothing to sign at $Path."
}

if (-not (Get-Command trusted-signing-cli -ErrorAction SilentlyContinue)) {
    throw "sign-windows: trusted-signing-cli is not on PATH. Install it with 'cargo install trusted-signing-cli'."
}

$resolved = (Resolve-Path -LiteralPath $Path).Path
Write-Host "sign-windows: signing $resolved"

# -d/-u populate the description and description URL shown in the UAC and
# SmartScreen prompts. Without them Windows falls back to the bare file name.
trusted-signing-cli `
    -e $endpoint `
    -a $account `
    -c $profileName `
    -d "Cubby Clipboard" `
    -u "https://cubbyclipboard.com" `
    $resolved

if ($LASTEXITCODE -ne 0) {
    throw "sign-windows: trusted-signing-cli failed for $resolved with exit code $LASTEXITCODE."
}

$signature = Get-AuthenticodeSignature -LiteralPath $resolved
if ($signature.Status -ne "Valid") {
    throw "sign-windows: $resolved is '$($signature.Status)' after signing: $($signature.StatusMessage)"
}

Write-Host "sign-windows: signed $resolved ($($signature.SignerCertificate.Subject))"
