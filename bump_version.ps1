param (
    [string]$Type = "patch"
)

# 1. Bump package.json version
Write-Host "Bumping package.json version ($Type)..."
npm version $Type --no-git-tag-version

if ($LASTEXITCODE -ne 0) {
    Write-Error "Failed to bump npm version"
    exit 1
}

# 2. Read the new version
$subVersion = (Get-Content -Path .\package.json | ConvertFrom-Json).version
Write-Host "New version is: $subVersion"

# 3. Update tauri.conf.json (string replace only — ConvertTo-Json rewrites formatting)
Write-Host "Updating tauri.conf.json..."
$tauriConfPath = ".\src-tauri\tauri.conf.json"
$tauriContent = Get-Content -Path $tauriConfPath -Raw
if ($tauriContent -notmatch '"version"\s*:\s*"[^"]+"') {
    Write-Error "Could not find version field in tauri.conf.json"
    exit 1
}
$tauriContent = [regex]::Replace($tauriContent, '"version"\s*:\s*"[^"]+"', ('"version": "' + $subVersion + '"'), 1)
Set-Content -Path $tauriConfPath -Value $tauriContent -NoNewline

# 4. Update Cargo.toml
Write-Host "Updating Cargo.toml..."
$cargoTomlPath = ".\src-tauri\Cargo.toml"
(Get-Content -Path $cargoTomlPath) -replace '^version = ".*"', ('version = "' + $subVersion + '"') | Set-Content -Path $cargoTomlPath

# 5. Sync Cargo.lock package version (required for cargo --locked on CI)
Write-Host "Syncing Cargo.lock..."
Push-Location src-tauri
cargo metadata --format-version 1 --no-deps | Out-Null
if ($LASTEXITCODE -ne 0) {
    Pop-Location
    Write-Error "Failed to sync Cargo.lock after version bump"
    exit 1
}
Pop-Location

Write-Host "Version bumped to $subVersion in all files."
Write-Host "You can now commit and tag: git commit -am 'v$subVersion' && git tag v$subVersion"
