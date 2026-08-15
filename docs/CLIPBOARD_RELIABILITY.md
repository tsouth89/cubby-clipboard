# Clipboard reliability contract

Clipboard capture is Cubby's primary product promise. A polished history UI is not useful if copied content is silently missed.

## Required behavior

- Listen for Windows clipboard changes through the native notification mechanism rather than periodic polling.
- Process clipboard sequence numbers in order and detect observable gaps.
- Retry short-lived clipboard access contention with bounded backoff.
- Enumerate every advertised clipboard format before choosing previews or normalized representations.
- Materialize delayed-rendered data while its owner is still available.
- Preserve the original formats needed for lossless paste, alongside searchable and preview-friendly representations.
- Commit a captured item atomically so partially read content never appears as a successful capture.
- Avoid recording Cubby's own clipboard writes as duplicate history entries.
- Continue capturing after sleep, unlock, explorer restart, remote-session reconnect, and clipboard-owner failure.
- Record local diagnostics for failed captures without collecting clipboard content or sending telemetry.
- Keep a supervised native listener that restarts on failure instead of exiting the process thread silently.
- Detect silent listener death with a clipboard-sequence watchdog (recreate when the sequence advances but no `WM_CLIPBOARDUPDATE` arrives for several seconds).
- Expose lightweight capture health via `get_clipboard_capture_status` (state, restart count, last error) without clipboard payloads.
- Keep local history durable: on startup, run SQLite `PRAGMA quick_check` on the existing `cubby.db`. If the file cannot be opened or fails the check, quarantine it (and any `-wal`/`-shm` sidecars) under a timestamped `*.corrupt-*` name and start a fresh empty database so capture never blocks on a broken store.
- Refresh a rolling `cubby.db.bak` snapshot of a healthy database at most once per 24 hours (after a WAL checkpoint), both when the database opens and while Cubby stays running, so operators have a recent recovery point without copying on every launch or requiring a quit.
- Never log clipboard content, previews, or decryptable payloads during recovery; logs may include paths and short structural diagnostics only. The DPAPI-protected `storage.key` and image blobs are left in place when the DB is quarantined.

## Format baseline

The first compatibility baseline covers:

- Unicode and legacy text
- HTML
- RTF
- PNG and Windows bitmap variants
- Multiple simultaneous formats representing the same copied item

File clipboard payloads are intentionally ignored. Windows file copies are
references to external paths rather than durable clipboard content, so Cubby
does not present them as stored history.

Application-specific formats should be preserved when practical and must not prevent standard formats from being captured.

## Remote-session matrix

Cubby must be exercised with:

- Windows Remote Desktop Connection
- Windows App / modern RDP client
- NinjaOne remote access used in normal support workflows
- At least one additional remote-control product with clipboard synchronization

For each client, test:

- remote-to-local and local-to-remote copies
- rapid sequences of distinct copies
- text, rich text, HTML, and images
- copies immediately before disconnect and immediately after reconnect
- repeated identical content
- clipboard redirection being disabled, enabled, or interrupted
- remote and local applications copying at nearly the same time

## Initial acceptance criteria

- No missed item in a 100-copy automated local burst at the fastest rate supported by the test harness.
- No missed text item in a 50-copy remote-session run under normal network conditions.
- Captured content remains available after the source application or remote session closes.
- Rich content can be pasted back into a compatible application without being reduced to plain text.
- A capture failure is visible in local diagnostics and never silently reported as successful.

These targets are a starting contract. Results from real applications and remote products should tighten the implementation and expand the regression suite.

## Capture probe

The Windows-only capture probe exercises the native clipboard notification path without involving the application UI:

```powershell
Push-Location src-tauri

# Automated local burst. Every marker must produce its own clipboard update.
cargo run --bin clipboard_probe -- --burst 100 --interval-ms 25

# Force the reader through clipboard-lock contention after every update.
cargo run --bin clipboard_probe -- --burst 100 --interval-ms 10 --contention-ms 40

# Interactive mode for RDP, NinjaOne, and other application testing.
cargo run --bin clipboard_probe -- --timeout-seconds 300

# Require 20 distinct readable text copies while ignoring remote sync churn.
cargo run --bin clipboard_probe -- --expect-text 20 --timeout-seconds 300

# Require 20 distinct text or screenshot copies.
cargo run --bin clipboard_probe -- --expect-items 20 --timeout-seconds 300

Pop-Location
```

Each event is emitted as JSON containing the clipboard sequence number, advertised formats, text or image status, dimensions or length, and a SHA-256 digest. Clipboard contents and image bytes are not printed. Burst mode exits unsuccessfully if it misses a marker, cannot read an update, or reaches the timeout. `--expect-text` counts distinct readable text. `--expect-items` counts distinct readable text or images and ignores non-content synchronization updates.

For repeatable local validation, run `scripts/test-clipboard-capture.ps1`. It
executes both rapid and intentionally contended bursts. Because Windows exposes
one system clipboard per interactive session, this test replaces the current
clipboard contents and should only be run when the keyboard is idle.

Run `scripts/test-clipboard-formats.ps1` for the deterministic multi-format
fixtures. A separate writer process publishes three atomic payloads and the
listener verifies both decoded values and exact bytes:

- Unicode text (including leading/trailing whitespace) with simultaneous CF_HTML
  and RTF payloads
- Unicode text with an ordered multi-item `CF_HDROP` list containing a real
  whitespace-sensitive text file, a Unicode-named binary file, and a folder
- Unicode text with an application-defined binary format containing embedded NUL
  and high bytes
- Delayed-rendered Unicode text with simultaneous CF_HTML and RTF payloads,
  materialized only when the listener requests each format
- A delayed-rendered 2x2 bitmap with exact RGBA pixel assertions
- Virtual-file-only and virtual-file-plus-text-fallback payloads, both classified
  through the same production policy as intentionally ignored file history

Fixture validation retries transient auxiliary-format read contention with
bounded exponential backoff. The writer retains each payload long enough for the
complete retry window, so a retry cannot accidentally validate the next fixture.
Physical targets stay on disk while the listener verifies their order, names,
types, and exact bytes, then the writer removes the isolated temporary directory.

The delayed writer owns a hidden Win32 window and responds to `WM_RENDERFORMAT`,
so those fixtures exercise genuine on-demand rendering rather than a writer-side
sleep. The virtual-file fixtures advertise `FileGroupDescriptorW`; their text
fallback is deliberately not captured because it is normally a path or display
name for content Cubby cannot retain durably.

The delayed-rendering owner is retired deliberately rather than just destroyed:
its payloads are dropped, clipboard ownership is released, and the message queue
is drained before the next fixture is written. Skipping any of that reintroduces
issue #164, where the next writer's `EmptyClipboard` made Windows send
`WM_RENDERALLFORMATS` to the still-owning window, whose handler opened and
closed the clipboard on the same thread -- closing the one `clipboard_rs` was
using and failing the `virtual_only` fixture with `ERROR_CLIPBOARD_NOT_OPEN`
(1418) roughly once every fifteen runs. The handler now also returns without
touching the clipboard when it has nothing left to render.

This suite proves the local Windows clipboard can materialize supported payloads
without normalization or format loss and can reject unsupported file payloads
without confusing them for stored history. Whether real applications accept
those payloads is a separate question, answered by the automated compatibility
matrix in `docs/COMPAT_MATRIX.md`; passing the fixtures on its own does not
claim that Office, browsers, remote clients, or elevated targets have been
validated.

Production materialization uses the same bounded retry margin: ten attempts with
exponential backoff capped at 64 ms, allowing up to 319 ms for a clipboard owner
or synchronization client to release the clipboard. The fixture writer takes the
clipboard on the same terms when it publishes a delayed payload, so probe
failures reflect capture behavior rather than a writer that gave up sooner than
production would have.
