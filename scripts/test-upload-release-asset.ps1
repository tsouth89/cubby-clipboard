$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$previousRepository = $env:GITHUB_REPOSITORY
$previousToken = $env:GH_TOKEN
$assetFile = New-TemporaryFile
$script:requests = [System.Collections.Generic.List[object]]::new()

function Invoke-RestMethod {
    param(
        [Parameter(Mandatory = $true)]
        [Uri]$Uri,

        [string]$Method,
        [hashtable]$Headers,
        [string]$ContentType,
        [string]$InFile
    )

    $script:requests.Add([pscustomobject]@{
        Uri         = $Uri
        Method      = $Method
        ContentType = $ContentType
        InFile      = $InFile
    })

    if ($Uri.Host -eq "api.github.com") {
        return @()
    }
    if ($Uri.Host -eq "uploads.github.com") {
        return [pscustomobject]@{ id = 123 }
    }

    throw "Unexpected request to $Uri"
}

try {
    Set-Content -LiteralPath $assetFile -Value "x" -NoNewline
    $env:GITHUB_REPOSITORY = "tsouth89/cubby-clipboard"
    $env:GH_TOKEN = "test-token"

    ./scripts/upload-release-asset.ps1 -ReleaseId 42 -AssetPath $assetFile

    if ($script:requests.Count -ne 2) {
        throw "Expected an empty asset-list request followed by one upload, got $($script:requests.Count) requests."
    }
    if ($script:requests[0].Uri.AbsoluteUri -ne "https://api.github.com/repos/tsouth89/cubby-clipboard/releases/42/assets?per_page=100") {
        throw "The helper did not list assets by release ID."
    }
    if ($script:requests[1].Uri.Host -ne "uploads.github.com") {
        throw "The helper did not use GitHub's release upload host."
    }
    if ($script:requests[1].ContentType -ne "application/octet-stream") {
        throw "The helper did not upload the asset as binary data."
    }

    Write-Host "Release asset upload handles a new draft with no existing assets."
} finally {
    $env:GITHUB_REPOSITORY = $previousRepository
    $env:GH_TOKEN = $previousToken
    Remove-Item -LiteralPath $assetFile -Force
}
