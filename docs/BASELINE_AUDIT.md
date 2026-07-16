# PastePaw baseline audit

Audit date: 2026-07-16

Fork point: `XueshiQiao/PastePaw@c05a4f849dbfd30c8143a891276df624a7e248b4`

## Verified baseline

- `pnpm install --frozen-lockfile`: passed
- `pnpm build`: passed
- `cargo check --locked`: passed
- `cargo test --locked`: passed, but discovered zero tests
- Frontend production bundle: approximately 510 KB minified JavaScript before Cubby cleanup

## Reusable foundations

- Rust and Tauri Windows application shell
- SQLite persistence through SQLx
- Text and image clipboard capture
- Global shortcut registration
- Multi-monitor popup placement
- Mica and Mica Alt effects
- Source-application detection
- Search, folders, exclusions, settings, tray, and autostart behavior

## Immediate removals

- Aptabase telemetry and startup event tracking
- Network-backed AI processing, API-key settings, and AI UI
- PastePaw update endpoint and signing key
- PastePaw package identity and data-directory names

## Architecture risks

1. Clipboard capture relies on inherited abstractions and heuristics rather than a clearly owned `WM_CLIPBOARDUPDATE` pipeline.
2. The item model does not preserve all simultaneous Windows clipboard formats losslessly.
3. Search uses SQLite `LIKE`, not FTS5.
4. Clipboard history and image blobs are not encrypted.
5. Automatic paste assumes synthetic keyboard behavior that will not work consistently across all Windows targets.
6. Source attribution can race the actual clipboard owner.
7. The WebView2 shell still needs to prove focus preservation, accessibility, keyboard behavior, DPI handling, and first-party visual fidelity.
8. The custom `window-vibrancy` dependency comes from an upstream-author fork and should be replaced or pinned to a reviewed source.
9. There are no meaningful automated tests or application compatibility harness.
10. RDP and third-party remote-control clients can redirect clipboard content through delayed rendering and format negotiation; the inherited polling approach is not sufficient evidence that these copies will be captured reliably.

## Architecture decision gate

Build equivalent minimal popup prototypes in:

- the cleaned Tauri/WebView2 shell
- WinUI 3 using Windows App SDK

Measure shortcut-to-visible latency, focus restoration, keyboard handling, Narrator output, high contrast, DPI and monitor placement, idle memory, paste reliability, and clipboard capture reliability across local and remote sessions. Keep Tauri only if it meets the native interaction contract without fragile workarounds.

The clipboard implementation must use the Windows clipboard notification path rather than interval polling, enumerate all advertised formats, handle delayed rendering, retry short-lived clipboard contention, and snapshot content before a remote client or source application replaces it.
