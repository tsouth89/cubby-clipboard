<#
.SYNOPSIS
    Verifies that the cubby.exe packed inside a built NSIS installer is signed.

.DESCRIPTION
    Do not check src-tauri/target/<triple>/release/cubby.exe for a signature
    after a bundle: it will report NotSigned even on a correctly signed build.
    Tauri patches and signs the app binary, packs it, and then restores the
    unsigned original so the next package type starts from clean bytes. Its own
    comment in tauri-bundler's bundle.rs says so:

        We make a copy of the unsigned main_binary so that we can restore it
        after each package_type step.

    The only trustworthy check is on the bytes users actually receive, so this
    extracts the installer and verifies the cubby.exe inside it.

    Extraction uses 7-Zip, which reads the NSIS container. It is preinstalled on
    GitHub's windows runners.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$Installer,

    # When set, the signer's certificate subject must contain this string. Guards
    # against a build that is signed, but by the wrong certificate profile.
    [string]$ExpectedSubject
)

$ErrorActionPreference = "Stop"

if (-not (Test-Path -LiteralPath $Installer)) {
    throw "verify-installer-signature: no installer at $Installer."
}

$sevenZip = Get-Command 7z -ErrorAction SilentlyContinue
if (-not $sevenZip) {
    $fallback = "C:\Program Files\7-Zip\7z.exe"
    if (Test-Path -LiteralPath $fallback) {
        $sevenZip = $fallback
    } else {
        throw "verify-installer-signature: 7z is not available to extract the installer."
    }
} else {
    $sevenZip = $sevenZip.Source
}

$resolved = (Resolve-Path -LiteralPath $Installer).Path
$extractDir = Join-Path ([System.IO.Path]::GetTempPath()) ("cubby-verify-" + [System.IO.Path]::GetFileNameWithoutExtension($resolved))
if (Test-Path -LiteralPath $extractDir) {
    Remove-Item -LiteralPath $extractDir -Recurse -Force
}

Write-Host "verify-installer-signature: extracting $resolved"
# 7-Zip returns 1 for non-fatal warnings on NSIS containers, so the presence of
# cubby.exe below is the real success condition rather than the exit code.
& $sevenZip x $resolved "-o$extractDir" -y | Out-Null

$app = Get-ChildItem -LiteralPath $extractDir -Filter "cubby.exe" -Recurse -File |
    Select-Object -First 1
if (-not $app) {
    throw "verify-installer-signature: no cubby.exe inside $resolved (7-Zip exit code $LASTEXITCODE)."
}

$signature = Get-AuthenticodeSignature -LiteralPath $app.FullName
if ($signature.Status -ne "Valid") {
    throw "verify-installer-signature: the packed cubby.exe is '$($signature.Status)' - signCommand did not sign the binary that shipped. $($signature.StatusMessage)"
}

$subject = $signature.SignerCertificate.Subject
if ($ExpectedSubject -and $subject -notlike "*$ExpectedSubject*") {
    throw "verify-installer-signature: the packed cubby.exe is signed by '$subject', which does not contain '$ExpectedSubject'."
}

Write-Host "verify-installer-signature: packed cubby.exe is signed by $subject"
Remove-Item -LiteralPath $extractDir -Recurse -Force -ErrorAction SilentlyContinue
