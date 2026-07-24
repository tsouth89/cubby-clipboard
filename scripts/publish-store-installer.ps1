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
    [switch]$SkipPublicVerification
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

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
            if ($existingHash -ne $candidateHash) {
                throw "Refusing to overwrite immutable release object with different bytes: $objectKey"
            }
            Write-Host "Immutable release object already has the expected bytes; skipping upload: $objectKey"
            return $false
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
