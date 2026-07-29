[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$CurrentPackagePath,

    [Parameter(Mandatory = $true)]
    [ValidatePattern('^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$')]
    [string]$Version,

    [string]$DownloadOrigin = "https://downloads.cubbyclipboard.com",

    [Parameter(Mandatory = $true)]
    [string]$OutputPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

if (-not (Test-Path -LiteralPath $CurrentPackagePath -PathType Leaf)) {
    throw "Current Microsoft Store package JSON not found: $CurrentPackagePath"
}

try {
    $current = Get-Content -LiteralPath $CurrentPackagePath -Raw | ConvertFrom-Json
} catch {
    throw "Microsoft Store CLI did not return valid package JSON: $($_.Exception.Message)"
}

$packagesProperty = $current.PSObject.Properties |
    Where-Object { $_.Name -ieq "packages" } |
    Select-Object -First 1
if (-not $packagesProperty) {
    throw "Microsoft Store package JSON does not contain a Packages array."
}

$packages = @($packagesProperty.Value)
if ($packages.Count -ne 2) {
    throw "Expected exactly two Microsoft Store packages for Cubby, but found $($packages.Count)."
}

$origin = $DownloadOrigin.TrimEnd('/')
$expectedArchitectures = @("x64", "arm64")
foreach ($architecture in $expectedArchitectures) {
    $storeArchitecture = if ($architecture -eq "x64") { "X64" } else { "Arm64" }
    $matches = @($packages | Where-Object {
        $architecturesProperty = $_.PSObject.Properties |
            Where-Object { $_.Name -ieq "architectures" } |
            Select-Object -First 1
        $architecturesProperty -and
            @($architecturesProperty.Value | ForEach-Object { "$_" }) -icontains $storeArchitecture
    })
    if ($matches.Count -ne 1) {
        throw "Expected exactly one $storeArchitecture Microsoft Store package, but found $($matches.Count)."
    }

    $packageUrlProperty = $matches[0].PSObject.Properties |
        Where-Object { $_.Name -ieq "packageUrl" } |
        Select-Object -First 1
    if (-not $packageUrlProperty) {
        throw "The $storeArchitecture Microsoft Store package does not contain a PackageUrl field."
    }

    $installerName = "Cubby.Clipboard_${Version}_${architecture}-Store-setup.exe"
    $packageUrlProperty.Value = "$origin/releases/v$Version/$installerName"
}

$outputDirectory = Split-Path -Parent $OutputPath
if ($outputDirectory -and -not (Test-Path -LiteralPath $outputDirectory)) {
    New-Item -ItemType Directory -Path $outputDirectory -Force | Out-Null
}

$current | ConvertTo-Json -Depth 100 | Set-Content -LiteralPath $OutputPath -Encoding utf8

$prepared = Get-Content -LiteralPath $OutputPath -Raw | ConvertFrom-Json
$preparedPackagesProperty = $prepared.PSObject.Properties |
    Where-Object { $_.Name -ieq "packages" } |
    Select-Object -First 1
$preparedPackages = @($preparedPackagesProperty.Value)
foreach ($architecture in $expectedArchitectures) {
    $installerName = "Cubby.Clipboard_${Version}_${architecture}-Store-setup.exe"
    $expectedUrl = "$origin/releases/v$Version/$installerName"
    $matchingUrls = @($preparedPackages | ForEach-Object {
        $urlProperty = $_.PSObject.Properties |
            Where-Object { $_.Name -ieq "packageUrl" } |
            Select-Object -First 1
        if ($urlProperty) { "$($urlProperty.Value)" }
    } | Where-Object { $_ -eq $expectedUrl })
    if ($matchingUrls.Count -ne 1) {
        throw "Prepared Microsoft Store submission does not contain exactly one expected $architecture URL."
    }
}

Write-Host "Prepared Microsoft Store package update for Cubby $Version (x64 and ARM64)."
