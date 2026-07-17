$ErrorActionPreference = 'Stop'

$releaseTargets = @(
    'x86_64-pc-windows-msvc',
    'aarch64-pc-windows-msvc'
)

foreach ($target in $releaseTargets) {
    # Cargo writes download/progress messages to stderr. Keep those out of the
    # parsed tree so an inert lockfile download cannot look like a reachable node.
    $tree = cargo tree --manifest-path src-tauri/Cargo.toml --locked --target $target | Out-String
    if ($LASTEXITCODE -ne 0) {
        throw "Failed to inspect the Rust dependency tree for $target."
    }

    if ($tree -match '(?m)(?:^|\s)rsa v0\.9\.10(?:\s|$)') {
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
