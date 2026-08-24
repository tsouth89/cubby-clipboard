param(
    [Parameter(Mandatory = $true)]
    [long]$ReleaseId,

    [Parameter(Mandatory = $true)]
    [string[]]$AssetPath
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

if ([string]::IsNullOrWhiteSpace($env:GITHUB_REPOSITORY)) {
    throw "GITHUB_REPOSITORY is required."
}
if ([string]::IsNullOrWhiteSpace($env:GH_TOKEN)) {
    throw "GH_TOKEN is required."
}

$apiHeaders = @{
    Accept                 = "application/vnd.github+json"
    Authorization          = "Bearer $env:GH_TOKEN"
    "X-GitHub-Api-Version" = "2022-11-28"
}
$assetListUri = "https://api.github.com/repos/$env:GITHUB_REPOSITORY/releases/$ReleaseId/assets?per_page=100"

foreach ($pathValue in $AssetPath) {
    $file = Get-Item -LiteralPath $pathValue
    $assets = @(Invoke-RestMethod -Uri $assetListUri -Headers $apiHeaders | Where-Object { $null -ne $_ })

    foreach ($asset in $assets | Where-Object { $_.name -eq $file.Name }) {
        $deleteUri = "https://api.github.com/repos/$env:GITHUB_REPOSITORY/releases/assets/$($asset.id)"
        Invoke-RestMethod -Uri $deleteUri -Method Delete -Headers $apiHeaders | Out-Null
    }

    $encodedName = [Uri]::EscapeDataString($file.Name)
    $uploadUri = "https://uploads.github.com/repos/$env:GITHUB_REPOSITORY/releases/$ReleaseId/assets?name=$encodedName"
    Invoke-RestMethod `
        -Uri $uploadUri `
        -Method Post `
        -Headers $apiHeaders `
        -ContentType "application/octet-stream" `
        -InFile $file.FullName | Out-Null

    Write-Host "Uploaded $($file.Name) to release $ReleaseId."
}
