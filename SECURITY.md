# Security

## Reporting a vulnerability

Please report suspected vulnerabilities through GitHub's private security-advisory flow. Do not include clipboard contents, secrets, or private logs in a public issue.

## Release security gates

Cubby release candidates must pass the JavaScript production dependency audit, the Rust advisory audit, packaged-build smoke tests, and the privacy checks in `scripts/check-release.mjs`.

### RUSTSEC-2023-0071 waiver

`Cargo.lock` currently records `rsa 0.9.10` through SQLx's disabled optional MySQL dependency. Cubby configures SQLx with default features disabled and enables SQLite only. `cargo tree --target all` confirms that RSA is not reachable in Cubby's active dependency graph.

`scripts/audit-rust.ps1` permits this one lockfile-only advisory and fails if RSA becomes reachable on any target. The waiver must be removed if SQLx stops recording the inactive package or if Cubby enables another SQLx database driver.

## Clipboard history at rest

Cubby encrypts clipboard payloads, previews, source attribution, metadata, and image files with AES-256-GCM. Dedupe values use a keyed HMAC rather than a plain content hash. The random storage key is protected for the current Windows user with DPAPI and is never stored in plaintext.

Existing plaintext history is migrated before the clipboard listener starts. Cubby fails closed if the key cannot be unlocked or migration cannot complete, preventing new history from being mixed into an unreadable or partially encrypted store.

Core Windows clipboard representations are retained together: Unicode text, HTML, RTF, file-drop lists, and images. Auxiliary formats are encrypted in the same authenticated store. Cubby intentionally does not persist arbitrary private application formats because some contain process-specific handles or unsafe opaque data that cannot be replayed reliably.
