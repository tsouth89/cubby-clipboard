# Automated portion of the packaged-release smoke gate (SOU-226).
# Run from the repository root on Windows before promoting a draft release.
#
# This script does not install the NSIS package (that remains a manual VM step
# in docs/RELEASE_CHECKLIST.md). It does fail closed on the automated checks
# that must be green before that manual pass starts.

$ErrorActionPreference = 'Stop'

function Invoke-Step([string]$Name, [scriptblock]$Action) {
    Write-Host ""
    Write-Host "==> $Name" -ForegroundColor Cyan
    & $Action
    if ($LASTEXITCODE -ne 0 -and $null -ne $LASTEXITCODE) {
        throw "Step failed: $Name (exit $LASTEXITCODE)"
    }
}

Push-Location (Split-Path -Parent $PSScriptRoot)
try {
    Invoke-Step 'Release metadata / CSP / capability / privacy gates' {
        pnpm run release:check
    }

    Invoke-Step 'JavaScript production dependency audit' {
        pnpm audit --prod
    }

    Invoke-Step 'Rust advisory audit (target-aware waiver)' {
        ./scripts/audit-rust.ps1
    }

    Invoke-Step 'Frontend production build' {
        pnpm run build
    }

    Invoke-Step 'Rust tests' {
        cargo test --manifest-path src-tauri/Cargo.toml --all-targets --locked
    }

    Invoke-Step 'Rust clippy (warnings denied)' {
        cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked -- -D warnings
    }

    Write-Host ""
    Write-Host "Automated smoke gate passed." -ForegroundColor Green
    Write-Host "Complete the packaged installer checks in docs/RELEASE_CHECKLIST.md next:"
    Write-Host "  - install / launch / autostart / update / uninstall on a clean Windows 11 VM"
    Write-Host "  - copy text + image, restore image, open Settings, updater/open-url"
    Write-Host "  - Win+V replacement toggle, pin/clear, restart persistence"
}
finally {
    Pop-Location
}
