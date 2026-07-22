# Cubby Clipboard release checklist

## Microsoft Store packages

Versioned Microsoft Store installers are served without redirects from
`https://downloads.cubbyclipboard.com/releases/v<version>/`. The `release`
environment must define:

- variable `CLOUDFLARE_ACCOUNT_ID`;
- variable `CLOUDFLARE_R2_BUCKET` (`cubby-downloads`); and
- secret `CLOUDFLARE_R2_API_TOKEN`, scoped to the Cubby bucket with R2 object
  write access.

Tag releases upload and verify both signed installers automatically. To backfill
an existing GitHub release, run the `Publish Microsoft Store packages` workflow
with its version tag. Submit the resulting immutable x64 and ARM64 URLs to
Microsoft Partner Center.

## Automated gate

Run from the repository root on Windows:

```powershell
./scripts/smoke-release.ps1
```

That script enforces:

- Release metadata consistency (`pnpm run release:check`), including CSP, scoped Tauri capabilities, encrypted-storage invariants, secret-aware privacy gates, and the no-`dangerouslySetInnerHTML` frontend check.
- JavaScript production dependency audit (`pnpm audit --prod`).
- Target-aware Rust advisory audit (`./scripts/audit-rust.ps1`), including the documented RSA waiver.
- Frontend production build.
- Rust tests and Clippy with warnings denied.

Also confirm:

- All three version fields match: `package.json`, `src-tauri/Cargo.toml`, and `src-tauri/tauri.conf.json`.
- Both x64 and arm64 NSIS installers build and are attached to the same draft release.

## Packaged-install smoke (manual VM)

After the automated gate is green, install the draft NSIS package on a clean Windows 11 VM and verify:

1. Install, first launch, tray presence, and cold startup.
2. Autostart enable/disable (installed builds only; portable builds must not write registry autostart).
3. Text copy → appears in history → Enter pastes into Notepad.
4. Screenshot/image copy → preview restores → Shift+Enter pastes OCR text when available.
5. Settings opens, persists a harmless toggle, and closes cleanly.
6. Updater check / About links open only the allow-listed HTTPS destinations.
7. `Win+V` replacement can be enabled and disabled without leaving Windows-key state stuck.
8. Rapid-copy burst, pinning, bulk clear, and restart persistence.
9. Local paste plus remote-desktop clipboard-sync paste with a large log when applicable.
10. Uninstall removes the app cleanly; document whether local history remains on disk.
11. Record SHA-256 hashes for the final installers.

## Public-release decisions

- Installers are signed with Azure Trusted Signing in CI; confirm SmartScreen reputation is acceptable for the channel you are announcing.
- Do not enable Winget publishing until `SouthForgeAI.CubbyClipboard` is reserved and the installer identity is final.
- Do not submit to Microsoft Store until Partner Center identity, signing, privacy text, and clean upgrade/uninstall behavior are verified.
- Keep GPL-3.0 source, `NOTICE.md`, and PastePaw attribution available with every release.
