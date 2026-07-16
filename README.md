# Cubby Clipboard

Cubby is an open-source clipboard history replacement for Windows 11. The goal is to preserve the familiar speed and simplicity of `Win+V` while adding reliable long-term history, instant search, richer clipboard formats, and clear privacy controls.

Cubby is in its foundation stage and is not ready for general use.

## Product principles

- Feel like part of Windows 11, not a cross-platform utility.
- Make the keyboard-first copy/paste loop instant and predictable.
- Capture clipboard changes reliably across local apps, RDP, and third-party remote-control sessions.
- Keep clipboard data local and private.
- Preserve copied content losslessly.
- Avoid required accounts, cloud services, telemetry, and AI features.
- Prefer reliability and compatibility over novelty.

## Current foundation

Cubby began as a fork of [PastePaw](https://github.com/XueshiQiao/PastePaw), which provided a useful Rust, Tauri, React, SQLite, global-shortcut, and clipboard-history baseline. The fork remains licensed under GPL-3.0 and preserves upstream history and attribution.

The inherited shell is being evaluated against a WinUI 3 prototype. Tauri is not a permanent architecture decision; Cubby will use the option that best meets the Windows focus, accessibility, rendering, performance, and packaging requirements.

## Relationship to Win+V

Cubby is designed as a focused replacement for Windows Clipboard History, not for every panel bundled into the Windows `Win+V` surface.

- Cubby uses `Win+V` by default and can release it in Settings. `Win+Period` remains the Windows shortcut for emoji, GIF, kaomoji, and symbol pickers.
- Selecting a clip pastes it into the previously focused app. In supported remote-control tools, Cubby may restore the synchronized clipboard and ask for a final `Ctrl+V` so large logs are not typed character by character.
- Clearing history preserves pinned clips by default. A separate confirmed action clears everything, including pins.
- History is local, searchable, and intended to extend well beyond Windows Clipboard History's short retention window.
- Cloud sync is intentionally absent until it can be offered with clear privacy and encryption guarantees.

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

Remote-session capture is a first-class reliability target. Cubby must be tested with Windows Remote Desktop and representative third-party remote-control tools, including rapid sequential copies, delayed-rendered content, reconnects, and text, image, HTML, RTF, and file-list formats.

Cubby includes a remote-session trigger and workflows optimized for remote
desktop and remote support tools, including large clipboard items. See
[Remote-session behavior](docs/REMOTE_SESSIONS.md).

## Repository layout

- `frontend/`: current React/Tauri interface
- `src-tauri/`: Rust clipboard, storage, windowing, and IPC implementation
- `docs/`: architecture findings and project notes

## Privacy

Cubby does not include PastePaw's Aptabase telemetry or network-backed AI integrations. Update infrastructure is also disabled until Cubby has its own signing keys and release process.

Clipboard history is not encrypted yet. Do not treat the current development build as suitable for sensitive clipboard data.

## License and attribution

Cubby is licensed under [GPL-3.0](LICENSE). See [NOTICE.md](NOTICE.md) for upstream attribution.
