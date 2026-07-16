# Cubby Clipboard

Cubby is an open-source clipboard history replacement for Windows 11. The goal is to preserve the familiar speed and simplicity of `Win+V` while adding reliable long-term history, instant search, richer clipboard formats, and clear privacy controls.

Cubby is in its foundation stage and is not ready for general use.

## Product principles

- Feel like part of Windows 11, not a cross-platform utility.
- Make the keyboard-first copy/paste loop instant and predictable.
- Keep clipboard data local and private.
- Preserve copied content losslessly.
- Avoid required accounts, cloud services, telemetry, and AI features.
- Prefer reliability and compatibility over novelty.

## Current foundation

Cubby began as a fork of [PastePaw](https://github.com/XueshiQiao/PastePaw), which provided a useful Rust, Tauri, React, SQLite, global-shortcut, and clipboard-history baseline. The fork remains licensed under GPL-3.0 and preserves upstream history and attribution.

The inherited shell is being evaluated against a WinUI 3 prototype. Tauri is not a permanent architecture decision; Cubby will use the option that best meets the Windows focus, accessibility, rendering, performance, and packaging requirements.

## Development

Requirements:

- Windows 11
- Node.js
- pnpm
- Rust with the MSVC toolchain
- Visual Studio C++ build tools and WebView2

```powershell
pnpm install --frozen-lockfile
pnpm build

Push-Location src-tauri
cargo check --locked
cargo test --locked
Pop-Location

pnpm tauri dev
```

The current upstream baseline has no meaningful automated tests. Adding a Windows clipboard compatibility harness is part of the planned foundation work.

## Repository layout

- `frontend/`: current React/Tauri interface
- `src-tauri/`: Rust clipboard, storage, windowing, and IPC implementation
- `docs/`: architecture findings and project notes

## Privacy

Cubby does not include PastePaw's Aptabase telemetry or network-backed AI integrations. Update infrastructure is also disabled until Cubby has its own signing keys and release process.

Clipboard history is not encrypted yet. Do not treat the current development build as suitable for sensitive clipboard data.

## License and attribution

Cubby is licensed under [GPL-3.0](LICENSE). See [NOTICE.md](NOTICE.md) for upstream attribution.
