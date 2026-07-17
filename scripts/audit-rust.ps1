$ErrorActionPreference = 'Stop'

$releaseTargets = @(
    'x86_64-pc-windows-msvc',
    'aarch64-pc-windows-msvc'
)

foreach ($target in $releaseTargets) {
    $tree = cargo tree --manifest-path src-tauri/Cargo.toml --target $target 2>&1 | Out-String
    if ($LASTEXITCODE -ne 0) {
        throw "Failed to inspect the Rust dependency tree for $target."
    }

    if ($tree -match '(?m)^[|`+\\\- ]*rsa v0\.9\.10(?:\s|$)') {
        throw "RUSTSEC-2023-0071 is reachable for $target; remove the waiver."
    }
}

# Cargo.lock includes SQLx optional MySQL driver packages even though Cubby builds
# SQLx with default features disabled and SQLite only. The tree check above makes
# this waiver fail closed if RSA ever becomes reachable in a release installer.
cargo audit --file src-tauri/Cargo.lock --ignore RUSTSEC-2023-0071
if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}
