# Cubby Clipboard release checklist

## Automated gate

- All three version fields match: `package.json`, `src-tauri/Cargo.toml`, and `src-tauri/tauri.conf.json`.
- Frontend formatting and production build pass.
- Rust formatting, all-target tests, and strict Clippy pass on Windows.
- Both x64 and arm64 NSIS installers build and are attached to the same draft release.

## Release-candidate test

1. Run the Release workflow manually to create a draft prerelease.
2. Install the x64 package on a clean Windows 11 VM and verify install, launch, autostart, upgrade, and uninstall.
3. Verify `Win+V` replacement can be enabled and disabled without leaving Windows-key state stuck.
4. Verify text, whitespace, screenshots, rapid-copy bursts, pinning, bulk clear, and restart persistence.
5. Verify local paste plus Ninja Remote clipboard-sync paste with a large log.
6. Confirm uninstall removes the app while preserving or removing history according to the installer choice presented to the user.
7. Record SHA-256 hashes for the final installers.

## Public-release decisions

- The first installer is unsigned. Document the expected SmartScreen warning prominently.
- Do not enable Winget publishing until `SouthForgeAI.CubbyClipboard` is reserved and the installer identity is final.
- Do not submit to Microsoft Store until Partner Center identity, signing, privacy text, and clean upgrade/uninstall behavior are verified.
- Do not enable automatic updates until updater signing keys, key rotation, and rollback behavior are documented and tested.
- Keep GPL-3.0 source, `NOTICE.md`, and PastePaw attribution available with every release.
