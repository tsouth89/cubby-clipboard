<#
.SYNOPSIS
    Verifies that the cubby.exe packed inside a built NSIS installer is signed,
    and that uninstall.exe is signed when 7-Zip actually extracts it.

.DESCRIPTION
    Do not check src-tauri/target/<triple>/release/cubby.exe for a signature
    after a bundle: it will report NotSigned even on a correctly signed build.
    Tauri patches and signs the app binary, packs it, and then restores the
    unsigned original so the next package type starts from clean bytes. Its own
    comment in tauri-bundler's bundle.rs says so:

        We make a copy of the unsigned main_binary so that we can restore it
        after each package_type step.

    The only trustworthy check is on the bytes users actually receive, so this
    extracts the installer and verifies the cubby.exe inside it. That is the
    installed app binary signCommand signs before packing. Store installers
    also embed a WebView2 bootstrapper; that file is not ours and is not
    required here.

    Tauri's NSIS uninstaller is produced at install time by WriteUninstaller
    and signed during makensis via !uninstfinalize. 7-Zip therefore usually
    cannot extract uninstall.exe from the container. If it is present, this
    script verifies it the same way as cubby.exe. If it is absent, that is
    not treated as "unsigned".

    A valid signature on the outer installer does not waive these checks. The
    Store publish and validate workflows keep the outer Authenticode / SHA
    checks as separate steps and call this script for the packed binaries.

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

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

# cubby.exe is packed with NSIS File and is the binary users run.
# uninstall.exe is optional in the extract: Tauri writes it at install time.
# SBS-777: do not treat a signed outer container as enough.
function Get-RequiredPackedExecutableNames {
    @("cubby.exe")
}

function Get-OptionalPackedExecutableNames {
    @("uninstall.exe")
}

# Pure decision so a test can pin "Valid outer, NotSigned inner" without 7-Zip
# or a real certificate. Callers still have to run the outer check themselves.
function Resolve-EmbeddedSignatureDecision {
    param(
        [Parameter(Mandatory = $true)]$Signature,
        [string]$ExpectedSubject
    )

    $status = [string]$Signature.Status
    if ($status -ne "Valid") {
        return "reject-status"
    }

    $subject = $null
    if ($Signature.PSObject.Properties["SignerCertificate"] -and $Signature.SignerCertificate) {
        $subject = [string]$Signature.SignerCertificate.Subject
    }
    if ([string]::IsNullOrWhiteSpace($subject)) {
        return "reject-missing-certificate"
    }
    if ($ExpectedSubject) {
        $escaped = [regex]::Escape($ExpectedSubject)
        if ($subject -notmatch ("^(?i)" + $escaped + "(,|$)")) {
            return "reject-subject"
        }
    }
    return "accept"
}

function Find-PackedExecutable {
    param(
        [Parameter(Mandatory = $true)][string]$ExtractDir,
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)][string]$InstallerPath,
        [int]$ExtractExitCode
    )

    $file = Get-ChildItem -LiteralPath $ExtractDir -Filter $Name -Recurse -File |
        Select-Object -First 1
    if (-not $file) {
        $installerName = Split-Path -Leaf $InstallerPath
        $hint = ""
        if ($PSBoundParameters.ContainsKey("ExtractExitCode")) {
            $hint = " (7-Zip exit code $ExtractExitCode)"
        }
        throw "verify-installer-signature: no $Name inside $installerName.$hint"
    }
    return $file
}

function Assert-PackedFileSignature {
    param(
        [Parameter(Mandatory = $true)][string]$InstallerPath,
        [Parameter(Mandatory = $true)][string]$EmbeddedPath,
        [string]$ExpectedSubject,
        [Parameter(Mandatory = $true)]$Signature
    )

    $embeddedName = Split-Path -Leaf $EmbeddedPath
    $installerName = Split-Path -Leaf $InstallerPath
    $decision = Resolve-EmbeddedSignatureDecision -Signature $Signature -ExpectedSubject $ExpectedSubject
    $status = [string]$Signature.Status

    if ($decision -eq "reject-status") {
        $detail = ""
        if ($Signature.PSObject.Properties["StatusMessage"] -and $Signature.StatusMessage) {
            $detail = " $([string]$Signature.StatusMessage)"
        }
        throw "verify-installer-signature: $installerName packed $embeddedName is '$status'.$detail"
    }

    if ($decision -eq "reject-missing-certificate") {
        throw "verify-installer-signature: $installerName packed $embeddedName is Valid but has no signer certificate."
    }

    $subject = [string]$Signature.SignerCertificate.Subject
    if ($decision -eq "reject-subject") {
        throw "verify-installer-signature: $installerName packed $embeddedName is signed by '$subject', which is not '$ExpectedSubject'."
    }

    return $subject
}

function Assert-EmbeddedPackageSignatures {
    param(
        [Parameter(Mandatory = $true)][string]$InstallerPath,
        [Parameter(Mandatory = $true)][string]$ExtractDir,
        [string]$ExpectedSubject,
        [scriptblock]$GetSignature,
        [int]$ExtractExitCode
    )

    if (-not $GetSignature) {
        $GetSignature = {
            param($Path)
            Get-AuthenticodeSignature -LiteralPath $Path
        }
    }

    $findArgs = @{
        ExtractDir = $ExtractDir
        Name = $null
        InstallerPath = $InstallerPath
    }
    if ($PSBoundParameters.ContainsKey("ExtractExitCode")) {
        $findArgs.ExtractExitCode = $ExtractExitCode
    }

    foreach ($name in Get-RequiredPackedExecutableNames) {
        $findArgs.Name = $name
        $file = Find-PackedExecutable @findArgs
        $signature = & $GetSignature $file.FullName
        $subject = Assert-PackedFileSignature `
            -InstallerPath $InstallerPath `
            -EmbeddedPath $file.FullName `
            -ExpectedSubject $ExpectedSubject `
            -Signature $signature
        Write-Host "verify-installer-signature: packed $name in $(Split-Path -Leaf $InstallerPath) is signed by $subject"
    }

    foreach ($name in Get-OptionalPackedExecutableNames) {
        $file = Get-ChildItem -LiteralPath $ExtractDir -Filter $name -Recurse -File |
            Select-Object -First 1
        if (-not $file) {
            Write-Host "verify-installer-signature: no $name inside $(Split-Path -Leaf $InstallerPath); Tauri generates the NSIS uninstaller at install time, so this is expected."
            continue
        }
        $signature = & $GetSignature $file.FullName
        $subject = Assert-PackedFileSignature `
            -InstallerPath $InstallerPath `
            -EmbeddedPath $file.FullName `
            -ExpectedSubject $ExpectedSubject `
            -Signature $signature
        Write-Host "verify-installer-signature: packed $name in $(Split-Path -Leaf $InstallerPath) is signed by $subject"
    }
}

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
$extractDir = Join-Path ([System.IO.Path]::GetTempPath()) ("cubby-verify-" + [guid]::NewGuid().ToString("N"))

try {
    New-Item -ItemType Directory -Path $extractDir | Out-Null
    Write-Host "verify-installer-signature: extracting $resolved"
    # 7-Zip returns 1 for non-fatal warnings on NSIS containers, so the presence
    # of the required packed executables is the real success condition rather
    # than the exit code. Keep the code in the missing-file error so a corrupt
    # extract is distinguishable from "extracted, but cubby.exe was not inside".
    & $sevenZip x $resolved "-o$extractDir" -y | Out-Null
    $extractExit = $LASTEXITCODE
    Assert-EmbeddedPackageSignatures `
        -InstallerPath $resolved `
        -ExtractDir $extractDir `
        -ExpectedSubject $ExpectedSubject `
        -ExtractExitCode $extractExit
}
finally {
    Remove-Item -LiteralPath $extractDir -Recurse -Force -ErrorAction SilentlyContinue
}
