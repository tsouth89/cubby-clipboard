# Tests the packed-binary signature rule used before a Store publish or
# backfill.
#
# The failure mode this pins is a correctly signed outer installer that still
# contains an unsigned cubby.exe or uninstall.exe. Store workflows used to
# treat the outer Authenticode / SHA checks as sufficient. Deliberately not a
# Pester suite: the runners and this machine disagree about which Pester major
# version is present, and the rule is a pure function plus a fixture directory.

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$script = Join-Path $PSScriptRoot "verify-installer-signature.ps1"
if (-not (Test-Path -LiteralPath $script)) {
    throw "verify-installer-signature.ps1 not found next to this test"
}

# Load the functions without running the script: dot-sourcing would demand the
# mandatory -Installer parameter and try to extract a real NSIS package.
$ast = [System.Management.Automation.Language.Parser]::ParseFile($script, [ref]$null, [ref]$null)
$functions = $ast.FindAll(
    { param($node) $node -is [System.Management.Automation.Language.FunctionDefinitionAst] },
    $true
)
foreach ($function in $functions) {
    . ([scriptblock]::Create($function.Extent.Text))
}

$failures = New-Object System.Collections.Generic.List[string]

function Assert-True {
    param([string]$What, [bool]$Condition)
    if ($Condition) {
        Write-Host "  ok   $What"
    }
    else {
        Write-Host "  FAIL $What" -ForegroundColor Red
        $failures.Add($What)
    }
}

function New-FakeSignature {
    param(
        [string]$Status,
        [string]$Subject,
        [string]$StatusMessage = ""
    )

    $certificate = $null
    if (-not [string]::IsNullOrWhiteSpace($Subject)) {
        $certificate = [pscustomobject]@{ Subject = $Subject }
    }
    [pscustomobject]@{
        Status = $Status
        StatusMessage = $StatusMessage
        SignerCertificate = $certificate
    }
}

$expectedSubject = "CN=Brandon South"
$validSubject = "CN=Brandon South, O=Brandon South, L=Wilmore, S=ky, C=US"

Write-Host "Get-RequiredPackedExecutableNames"

$required = @(Get-RequiredPackedExecutableNames)
$optional = @(Get-OptionalPackedExecutableNames)
Assert-True "requires cubby.exe" ($required -contains "cubby.exe")
Assert-True "does not require uninstall.exe in the 7-Zip extract" ($required -notcontains "uninstall.exe")
Assert-True "treats uninstall.exe as optional-if-present" ($optional -contains "uninstall.exe")
Assert-True "does not require the WebView2 bootstrapper" (
    @($required + $optional | Where-Object { $_ -like "*WebView*" }).Count -eq 0
)

Write-Host "Resolve-EmbeddedSignatureDecision"

# The case Store publication used to miss: a Valid outer signature does not
# change this function's answer. An unsigned packed cubby.exe is still reject.
$unsignedInner = New-FakeSignature -Status "NotSigned" -Subject "" -StatusMessage "The file is not signed."
Assert-True "unsigned inner is reject-status even when the caller already accepted the outer package" (
    (Resolve-EmbeddedSignatureDecision -Signature $unsignedInner -ExpectedSubject $expectedSubject) -eq "reject-status"
)

$validInner = New-FakeSignature -Status "Valid" -Subject $validSubject
Assert-True "valid expected subject is accept" (
    (Resolve-EmbeddedSignatureDecision -Signature $validInner -ExpectedSubject $expectedSubject) -eq "accept"
)

$wrongSubject = New-FakeSignature -Status "Valid" -Subject "CN=Someone Else"
Assert-True "unexpected signer is reject-subject" (
    (Resolve-EmbeddedSignatureDecision -Signature $wrongSubject -ExpectedSubject $expectedSubject) -eq "reject-subject"
)

$validNoSubject = New-FakeSignature -Status "Valid" -Subject ""
Assert-True "Valid status with no certificate is reject-status" (
    (Resolve-EmbeddedSignatureDecision -Signature $validNoSubject -ExpectedSubject $expectedSubject) -eq "reject-status"
)

Write-Host "Assert-PackedFileSignature / Assert-EmbeddedPackageSignatures"

$scratch = Join-Path ([System.IO.Path]::GetTempPath()) ("cubby-verify-test-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $scratch | Out-Null
try {
    $installerName = "Cubby.Clipboard_1.3.1_x64-Store-setup.exe"
    $installerPath = Join-Path $scratch $installerName
    Set-Content -LiteralPath $installerPath -Value "outer-installer-bytes"
    $extractDir = Join-Path $scratch "extracted"
    New-Item -ItemType Directory -Path $extractDir | Out-Null
    $cubbyPath = Join-Path $extractDir "cubby.exe"
    $uninstallPath = Join-Path $extractDir "uninstall.exe"
    Set-Content -LiteralPath $cubbyPath -Value "unsigned-cubby"
    Set-Content -LiteralPath $uninstallPath -Value "signed-uninstaller"

    # Signed outer package (the file exists; we never consult its signature
    # here) with an unsigned packed cubby.exe. This is the SBS-777 miss.
    $lookup = {
        param($Path)
        $leaf = Split-Path -Leaf $Path
        if ($leaf -eq "cubby.exe") {
            return New-FakeSignature -Status "NotSigned" -Subject "" -StatusMessage "The file is not signed."
        }
        return New-FakeSignature -Status "Valid" -Subject $validSubject
    }

    $rejectedUnsignedInner = $false
    $unsignedInnerMessage = ""
    try {
        Assert-EmbeddedPackageSignatures `
            -InstallerPath $installerPath `
            -ExtractDir $extractDir `
            -ExpectedSubject $expectedSubject `
            -GetSignature $lookup
    }
    catch {
        $rejectedUnsignedInner = $true
        $unsignedInnerMessage = [string]$_.Exception.Message
    }
    Assert-True "signed outer + unsigned cubby.exe is rejected" $rejectedUnsignedInner
    Assert-True "rejection names the installer artifact" ($unsignedInnerMessage -like "*$installerName*")
    Assert-True "rejection names the packed cubby.exe" ($unsignedInnerMessage -like "*cubby.exe*")
    Assert-True "rejection does not mention signing credentials" (
        $unsignedInnerMessage -notlike "*ARTIFACT_SIGNING*" -and
        $unsignedInnerMessage -notlike "*CLIENT_SECRET*"
    )

    $bothSigned = {
        param($Path)
        New-FakeSignature -Status "Valid" -Subject $validSubject
    }

    # Missing uninstall.exe must not fail: Tauri WriteUninstaller does not pack it.
    Remove-Item -LiteralPath $uninstallPath -Force
    $acceptedWithoutUninstaller = $true
    try {
        Assert-EmbeddedPackageSignatures `
            -InstallerPath $installerPath `
            -ExtractDir $extractDir `
            -ExpectedSubject $expectedSubject `
            -GetSignature $bothSigned
    }
    catch {
        $acceptedWithoutUninstaller = $false
        Write-Host "  FAIL missing uninstall.exe should be accepted: $($_.Exception.Message)" -ForegroundColor Red
        $failures.Add("missing uninstall.exe should be accepted")
    }
    Assert-True "missing uninstall.exe is accepted when cubby.exe is signed" $acceptedWithoutUninstaller

    Set-Content -LiteralPath $uninstallPath -Value "unsigned-uninstaller"
    $unsignedUninstaller = {
        param($Path)
        $leaf = Split-Path -Leaf $Path
        if ($leaf -eq "uninstall.exe") {
            return New-FakeSignature -Status "NotSigned" -Subject "" -StatusMessage "The file is not signed."
        }
        return New-FakeSignature -Status "Valid" -Subject $validSubject
    }
    $rejectedUnsignedUninstaller = $false
    $unsignedUninstallerMessage = ""
    try {
        Assert-EmbeddedPackageSignatures `
            -InstallerPath $installerPath `
            -ExtractDir $extractDir `
            -ExpectedSubject $expectedSubject `
            -GetSignature $unsignedUninstaller
    }
    catch {
        $rejectedUnsignedUninstaller = $true
        $unsignedUninstallerMessage = [string]$_.Exception.Message
    }
    Assert-True "extracted unsigned uninstall.exe is rejected" $rejectedUnsignedUninstaller
    Assert-True "unsigned-uninstaller error names uninstall.exe and the installer" (
        $unsignedUninstallerMessage -like "*uninstall.exe*" -and
        $unsignedUninstallerMessage -like "*$installerName*"
    )

    $accepted = $true
    try {
        Assert-EmbeddedPackageSignatures `
            -InstallerPath $installerPath `
            -ExtractDir $extractDir `
            -ExpectedSubject $expectedSubject `
            -GetSignature $bothSigned
    }
    catch {
        $accepted = $false
        Write-Host "  FAIL both-signed should accept: $($_.Exception.Message)" -ForegroundColor Red
        $failures.Add("both-signed should accept")
    }
    Assert-True "cubby.exe and a present uninstall.exe signed by the expected subject are accepted" $accepted

    $wrongLookup = {
        param($Path)
        New-FakeSignature -Status "Valid" -Subject "CN=Wrong Profile"
    }
    $rejectedWrongSubject = $false
    $wrongSubjectMessage = ""
    try {
        Assert-EmbeddedPackageSignatures `
            -InstallerPath $installerPath `
            -ExtractDir $extractDir `
            -ExpectedSubject $expectedSubject `
            -GetSignature $wrongLookup
    }
    catch {
        $rejectedWrongSubject = $true
        $wrongSubjectMessage = [string]$_.Exception.Message
    }
    Assert-True "unexpected packed signer is rejected" $rejectedWrongSubject
    Assert-True "wrong-subject error names the installer and packed file" (
        $wrongSubjectMessage -like "*$installerName*" -and
        ($wrongSubjectMessage -like "*cubby.exe*" -or $wrongSubjectMessage -like "*uninstall.exe*")
    )
}
finally {
    Remove-Item -LiteralPath $scratch -Recurse -Force -ErrorAction SilentlyContinue
}

if ($failures.Count -gt 0) {
    Write-Host ""
    throw "$($failures.Count) embedded-signature assertion(s) failed: $($failures -join ', ')"
}

Write-Host ""
Write-Host "embedded installer signatures: all assertions passed"
