# Security

## Reporting a vulnerability

Please report suspected vulnerabilities through GitHub's private security-advisory flow. Do not include clipboard contents, secrets, or private logs in a public issue.

## Release security gates

Cubby release candidates must pass the JavaScript production dependency audit, the Rust advisory audit, packaged-build smoke tests, and the privacy checks in `scripts/check-release.mjs`.

### RUSTSEC-2023-0071 waiver

`Cargo.lock` currently records `rsa 0.9.10` through SQLx's disabled optional MySQL dependency. Cubby configures SQLx with default features disabled and enables SQLite only. `cargo tree --target all` confirms that RSA is not reachable in Cubby's active dependency graph.

`scripts/audit-rust.ps1` permits this one lockfile-only advisory and fails if RSA becomes reachable on any target. The waiver must be removed if SQLx stops recording the inactive package or if Cubby enables another SQLx database driver.

## Current development-build limitation

Clipboard payloads are not yet encrypted at rest. Development builds must not be represented as suitable for retaining sensitive clipboard data until the encrypted-storage release gate is complete.
