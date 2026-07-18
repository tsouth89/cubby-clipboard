# Cubby inherited-baseline audit

Audit refreshed: 2026-07-18

Cubby began from `XueshiQiao/PastePaw` at commit
`c05a4f849dbfd30c8143a891276df624a7e248b4`. The upstream commits, authorship,
GPL-3.0 license, and contributor credit remain in this repository. Current
attribution details live in [`NOTICE.md`](../NOTICE.md).

This document tracks the engineering state of inherited subsystems. It is not
current product documentation; use the README and focused documents in this
folder for supported behavior.

## Current status

| Inherited area | Cubby status |
| --- | --- |
| Clipboard polling and debounce | Replaced with `AddClipboardFormatListener`, ordered capture, and bounded contention retries |
| Text/image-only storage model | Replaced with encrypted text, HTML, RTF, image, and file-list preservation |
| Plaintext clipboard storage | Replaced with AES-256-GCM payload encryption and a Windows DPAPI-protected key |
| Inline/best-effort screenshot OCR | Replaced with a durable, recoverable single-worker queue using local Windows OCR |
| SQLite/linear search | Still decrypts candidate rows and performs substring matching in Rust; encrypted-safe indexing remains planned |
| Generic synthetic paste | Replaced with target-aware paste behavior and dedicated local/remote compatibility harnesses |
| Generic popup placement | Replaced with cursor-anchored, monitor-aware Windows 11 flyout placement |
| Upstream telemetry and AI UI | Removed; the desktop app contains no analytics, cloud AI, or API-key configuration |
| Upstream updater and identity | Replaced with Cubby package identity, signed GitHub releases, and recurring update checks |
| Automated coverage | Rust unit/integration coverage plus frontend, website, analytics, release, dependency, and security checks |

## Remaining architecture work

1. Add an encrypted-at-rest-safe search index without persisting plaintext
   clipboard or OCR content.
2. Continue accessibility validation with Narrator, high contrast, keyboard-only
   navigation, DPI changes, and multi-monitor placement.
3. Expand clipboard and OCR fixtures across Windows applications, languages,
   RDP, and representative remote-support tools.
4. Keep the custom `window-vibrancy` source pinned and reviewed until it can be
   replaced with an appropriate maintained dependency.

The live acceptance evidence is recorded in `CAPTURE_PROBE_RESULTS.md`,
`PRODUCTION_CAPTURE_RESULTS.md`, `REMOTE_SESSIONS.md`, and the automated tests.
