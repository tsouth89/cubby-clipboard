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
with its version tag.

Microsoft Store submission is disabled until Partner Center onboarding has
been validated. The GitHub `release` environment must also define:

- variable `PARTNER_CENTER_PRODUCT_ID`;
- variable `MICROSOFT_STORE_SUBMISSION_ENABLED` (`false` during onboarding);
- secrets `PARTNER_CENTER_TENANT_ID`, `PARTNER_CENTER_SELLER_ID`,
  `PARTNER_CENTER_CLIENT_ID`, and `PARTNER_CENTER_CLIENT_SECRET`.

The manual **Validate Microsoft Store submission** workflow uses a separate
`store-validation` environment containing the same five Partner Center values.
It verifies both R2 installers, checksums, outer Authenticode signatures, and
the signature on the packed `cubby.exe` (and `uninstall.exe` when 7-Zip extracts it), authenticates to Partner Center,
and maps the existing x64 and ARM64 Store packages to their new versioned URLs
without changing a submission unless `publish` is explicitly selected.

After a validation-only run passes, set
`MICROSOFT_STORE_SUBMISSION_ENABLED=true`. Future tag releases wait for both
architecture uploads, update both package URLs together, and create one Store
submission before the GitHub release is published. Reruns skip existing R2
objects only when their bytes match; a different build must use a new version
instead of replacing an existing versioned URL, even while the GitHub release
is still a draft.

## Automated gate

Run from the repository root on Windows:

```powershell
./scripts/smoke-release.ps1
```

That script enforces:

- Release metadata consistency (`pnpm run release:check`), including CSP, scoped Tauri capabilities, encrypted-storage invariants, secret-aware privacy gates, the no-`dangerouslySetInnerHTML` frontend check, and the privileged-workflow action pin check.
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

## Lifecycle / recovery acceptance (SOU-218 remainder)

Capture-listener restart and the sequence watchdog already shipped. Confirm the
storage and session slices below on a daily-driver or VM before closing the
issue. Do not paste clipboard contents into bug reports or logs.

### Database repair / backup

1. Fresh install: `%AppData%` (or portable data dir) has no `cubby.db`; first
   launch creates it and capture works.
2. After history exists, quit Cubby and confirm a `cubby.db.bak` appears within
   one successful launch (rolling backup; may skip if a backup younger than
   24h already exists).
3. With Cubby quit, replace `cubby.db` with garbage bytes (or truncate mid-file).
   Relaunch: app must start, capture must work, and the bad file must be renamed
   to something like `cubby.db.corrupt-YYYYMMDD-HHMMSS`. `storage.key` must
   still be present. History is empty (expected after quarantine).
4. Logs under the app log dir mention quarantine/structural failure only; no
   clip text or image payloads.

### Single-instance and autostart

1. With Cubby running, start a second process (installer shortcut or
   `Cubby.exe`): only one process remains; the existing flyout shows near the
   cursor.
2. Enable autostart in Settings (installed build), sign out/in or reboot, confirm
   tray icon returns and a new copy appears in history.
3. Portable build: autostart control is unavailable / does not write Run-key
   entries.

### Display and session (verification; no required code change yet)

1. Copy text, then sleep/resume Windows; within ~10s another copy is captured
   (watchdog may take 5–7s after silent listener death).
2. Lock and unlock the session; capture still works.
3. Change display scale (100% ↔ 150%) or unplug a monitor with the flyout open
   or closed; next hotkey show positions on the active work area without
   crashing.
4. Move the taskbar (bottom ↔ side); flyout still clamps to the work area.
5. Optional: switch virtual desktop and invoke the hotkey; window appears on the
   current desktop.

### Capture health API

From a dev build or tooling, invoke `get_clipboard_capture_status` and confirm
fields are present (`state`, restart counters, last error) with no clipboard
payload. A Settings/About surface for this status is still optional.

## Public-release decisions

- Installers are signed with Azure Trusted Signing in CI; confirm SmartScreen reputation is acceptable for the channel you are announcing.
- After installing the built artifact, confirm every executable in the install
  directory is signed and that nothing unexpected shipped alongside the app:

  ```powershell
  Get-ChildItem "$env:LOCALAPPDATA\Cubby Clipboard\*.exe" |
    ForEach-Object { [PSCustomObject]@{ Name = $_.Name; Status = (Get-AuthenticodeSignature $_).Status } }
  ```

  Expect exactly `cubby.exe` and `uninstall.exe`, both `Valid`. An extra binary
  means a `src/bin` target lost its `required-features = ["dev-harness"]` gate;
  a `NotSigned` result means `signCommand` did not run.
- Do not enable Winget publishing until `SouthForgeAI.CubbyClipboard` is reserved and the installer identity is final.
- Do not submit to Microsoft Store until Partner Center identity, signing, privacy text, and clean upgrade/uninstall behavior are verified.
- Keep GPL-3.0 source, `NOTICE.md`, and PastePaw attribution available with every release.

## Privileged GitHub Action pins (SBS-778)

Release, Store, and signing workflows may only consume third-party Actions by
full commit SHA, with the human-readable version in an adjacent comment. First-party
`actions/*` refs may stay on a major tag. The release metadata check and CI run
`.github/scripts/check-privileged-action-pins.mjs` and reject a new mutable
third-party `uses:` line.

When Dependabot opens an actions pin update:

1. Read the action changelog and the commit range, not only the new SHA.
2. Confirm the pin is a 40-character commit object (not an annotated-tag SHA).
3. Confirm the adjacent `# version` comment still names the reviewed release.
4. Run `node .github/scripts/check-privileged-action-pins.mjs` and
   `node --test .github/scripts/check-privileged-action-pins.test.mjs`.
5. Merge the pin update. Do not retarget the `uses:` ref back to a floating
   tag such as `@v3` or `@stable`.

## Pre-merge Windows compile matrix (SBS-779)

Pull-request CI cargo-checks four shipped configurations on `windows-latest`:

- x64 default
- x64 `app-store`
- ARM64 default
- ARM64 `app-store`

That is a `cargo check --all-targets` per leg, not a bundle. Release still owns
NSIS packaging, signing, and the Store installer. `.github/scripts/check-shipped-windows-ci.mjs`
fails the required CI job if a leg is removed or renamed into an opaque matrix.

A configuration-specific compile break should now fail the PR (`Check <arch> <features>`)
instead of the first tag/release build that exercises that pair.

