[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$')]
    [string]$Version,

    [Parameter(Mandatory = $true)]
    [ValidateSet("x64", "arm64")]
    [string]$Architecture,

    [Parameter(Mandatory = $true)]
    [string]$InstallerPath,

    [string]$BucketName = "cubby-downloads",
    [string]$DownloadOrigin = "https://downloads.cubbyclipboard.com",
    [string]$WranglerVersion = "4.113.0",
    [string]$ExpectedSigner = "CN=Brandon South",
    [switch]$SkipPublicVerification,

    # Release tag, so this script can decide for itself whether replacing an
    # existing object is safe.
    #
    # Release objects are immutable once a version has shipped -- a download URL
    # must never change under a user. But the first run of a tag claims its
    # object keys permanently, and code signing is not deterministic, so a
    # release whose build failed part way could never be retried and recovery
    # always cost a version number (v1.3.0 was burned exactly this way).
    #
    # Replacement is therefore allowed while the tag's GitHub release is still a
    # draft. That check is made *here*, immediately before the replacement,
    # rather than accepted as a caller's assertion: a boolean decided several
    # steps earlier can be stale by the time it is used, and a script guarding
    # user-facing immutability should not be talked out of it by its caller.
    # Omit the tag and replacement is always refused.
    [string]$ReleaseTag
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

# Whether an existing release object may be replaced.
#
# Pure and side-effect free so the rule itself can be tested; everything that
# touches the network lives in its callers. `scripts/test-publish-guard.ps1`
# covers all three outcomes.
function Resolve-ImmutableObjectAction {
    param(
        # The object already present has the same bytes we would upload.
        [Parameter(Mandatory = $true)][bool]$BytesMatch,
        # The GitHub release for this tag has not been published yet.
        [Parameter(Mandatory = $true)][bool]$ReleaseIsDraft
    )

    if ($BytesMatch) { return "skip" }
    if ($ReleaseIsDraft) { return "replace" }
    return "refuse"
}

# True only when the tag has a GitHub release that is still a draft. Any doubt
# -- no tag, a gh failure, an unparseable answer -- resolves to false, which
# makes the caller refuse the replacement.
function Test-ReleaseIsDraft {
    param([string]$Tag)

    if ([string]::IsNullOrWhiteSpace($Tag)) { return $false }

    $isDraft = & gh release view $Tag --json isDraft --jq .isDraft 2>$null
    if ($LASTEXITCODE -ne 0) { return $false }
    return ("$isDraft".Trim() -eq "true")
}

$resolvedInstallerPath = (Resolve-Path -LiteralPath $InstallerPath).Path
$installerName = "Cubby.Clipboard_${Version}_${Architecture}-Store-setup.exe"
$hashName = "$installerName.sha256"
$objectPrefix = "releases/v$Version"
$localHash = (Get-FileHash -LiteralPath $resolvedInstallerPath -Algorithm SHA256).Hash.ToLowerInvariant()
$hashPath = Join-Path ([System.IO.Path]::GetTempPath()) "$hashName-$([guid]::NewGuid().ToString('N')).txt"

$installer = Get-Item -LiteralPath $resolvedInstallerPath
if ($installer.Length -lt 50MB) {
    throw "Store installer is only $($installer.Length) bytes. Expected an offline installer containing WebView2."
}

$signature = Get-AuthenticodeSignature -FilePath $resolvedInstallerPath
if ($signature.Status -ne "Valid") {
    throw "Store installer Authenticode signature is not valid. Status: $($signature.Status)"
}
if ($signature.SignerCertificate.Subject -notlike "*$ExpectedSigner*") {
    throw "Unexpected Store installer signer: $($signature.SignerCertificate.Subject)"
}
if ($null -eq $signature.TimeStamperCertificate) {
    throw "Store installer is missing an RFC 3161 timestamp."
}

function Test-R2ObjectRequiresUpload {
    param(
        [Parameter(Mandatory = $true)]
        [string]$ObjectName,

        [Parameter(Mandatory = $true)]
        [string]$Path
    )

    $objectKey = "$objectPrefix/$ObjectName"
    $origin = $DownloadOrigin.TrimEnd('/')
    $encodedObjectName = [uri]::EscapeDataString($ObjectName)
    $objectUrl = "$origin/$objectPrefix/$encodedObjectName"
    $probeUrl = "$objectUrl`?immutable-probe=$([guid]::NewGuid().ToString('N'))"
    $probePath = Join-Path ([System.IO.Path]::GetTempPath()) "cubby-r2-probe-$([guid]::NewGuid().ToString('N'))"
    try {
        $statusCode = & curl.exe `
            --silent `
            --show-error `
            --max-redirs 0 `
            --connect-timeout 10 `
            --max-time 180 `
            --output $probePath `
            --write-out "%{http_code}" `
            $probeUrl
        if ($LASTEXITCODE -ne 0) {
            throw "Could not check whether $objectUrl already exists."
        }
        if ($statusCode -eq "200") {
            $existingHash = (Get-FileHash -LiteralPath $probePath -Algorithm SHA256).Hash
            $candidateHash = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash
            if ($existingHash -eq $candidateHash) {
                Write-Host "Immutable release object already has the expected bytes; skipping upload: $objectKey"
                return $false
            }

            # Checked now rather than earlier: this is the last instant before
            # the object is replaced, so it is the only check that cannot have
            # gone stale in between.
            $action = Resolve-ImmutableObjectAction `
                -BytesMatch $false `
                -ReleaseIsDraft (Test-ReleaseIsDraft -Tag $ReleaseTag)
            if ($action -eq "replace") {
                Write-Warning "Replacing ${objectKey}: the bytes differ, but $ReleaseTag is still a draft, so nothing has shipped from this URL yet."
                return $true
            }

            throw "Refusing to overwrite immutable release object with different bytes: $objectKey. The release for this tag is published (or no tag was given), so this URL is live and must not change. Cut a new version instead."
        }
        if ($statusCode -ne "404") {
            throw "Unexpected HTTP $statusCode while checking $objectUrl."
        }
        return $true
    } finally {
        Remove-Item -LiteralPath $probePath -Force -ErrorAction SilentlyContinue
    }
}

function Publish-R2Object {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path,

        [Parameter(Mandatory = $true)]
        [string]$ObjectName,

        [Parameter(Mandatory = $true)]
        [string]$ContentType,

        [Parameter(Mandatory = $true)]
        [string]$CacheControl
    )

    if (-not (Test-R2ObjectRequiresUpload -ObjectName $ObjectName -Path $Path)) {
        return
    }
    $objectPath = "$BucketName/$objectPrefix/$ObjectName"
    & npx --yes "wrangler@$WranglerVersion" r2 object put $objectPath `
        "--file=$Path" `
        --remote `
        --force `
        "--content-type=$ContentType" `
        "--cache-control=$CacheControl"
    if ($LASTEXITCODE -ne 0) {
        throw "Wrangler failed to upload $ObjectName to R2."
    }
}

try {
    Set-Content -LiteralPath $hashPath -Value "$localHash  $installerName" -NoNewline

    Publish-R2Object `
        -Path $resolvedInstallerPath `
        -ObjectName $installerName `
        -ContentType "application/vnd.microsoft.portable-executable" `
        -CacheControl "public, max-age=31536000, immutable"
    Publish-R2Object `
        -Path $hashPath `
        -ObjectName $hashName `
        -ContentType "text/plain; charset=utf-8" `
        -CacheControl "public, max-age=31536000, immutable"

    $installerUrl = "$($DownloadOrigin.TrimEnd('/'))/$objectPrefix/$installerName"
    if ($SkipPublicVerification) {
        Write-Output "Uploaded Microsoft Store installer: $installerUrl"
        return
    }

    $downloadPath = Join-Path ([System.IO.Path]::GetTempPath()) "cubby-store-$Version-$Architecture-$([guid]::NewGuid().ToString('N')).exe"
    try {
        & curl.exe `
            --fail `
            --silent `
            --show-error `
            --location `
            --max-redirs 0 `
            --connect-timeout 10 `
            --max-time 180 `
            --retry 3 `
            --retry-delay 5 `
            --retry-connrefused `
            --output $downloadPath `
            "$($installerUrl)?verify=$([guid]::NewGuid().ToString('N'))"
        if ($LASTEXITCODE -ne 0) {
            throw "Direct public download failed with curl exit code $LASTEXITCODE."
        }

        $downloadHash = (Get-FileHash -LiteralPath $downloadPath -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($downloadHash -ne $localHash) {
            throw "Public installer SHA-256 mismatch. Expected $localHash, got $downloadHash."
        }
    } finally {
        Remove-Item -LiteralPath $downloadPath -Force -ErrorAction SilentlyContinue
    }

    Write-Output "Verified direct Microsoft Store installer URL: $installerUrl"
} finally {
    Remove-Item -LiteralPath $hashPath -Force -ErrorAction SilentlyContinue
}
