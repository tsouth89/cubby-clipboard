$ErrorActionPreference = 'Stop'

$tree = cargo tree --manifest-path src-tauri/Cargo.toml --target all 2>&1 | Out-String
if ($LASTEXITCODE -ne 0) {
    throw 'Failed to inspect the active Rust dependency tree.'
}

if ($tree -match '(?m)^.*rsa v0\.9\.10') {
    throw 'RUSTSEC-2023-0071 is reachable in the active dependency tree; remove the waiver.'
}

# Cargo.lock includes SQLx optional MySQL driver packages even though Cubby builds
# SQLx with default features disabled and SQLite only. The tree check above makes
# this waiver fail closed if RSA ever becomes reachable on any supported target.
cargo audit --file src-tauri/Cargo.lock --ignore RUSTSEC-2023-0071
if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}
