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

## Format baseline

The first compatibility baseline covers:

- Unicode and legacy text
- HTML
- RTF
- PNG and Windows bitmap variants
- File lists and virtual files
- Multiple simultaneous formats representing the same copied item

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
- text, rich text, HTML, images, and files where supported
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

# Interactive mode for RDP, NinjaOne, and other application testing.
cargo run --bin clipboard_probe -- --timeout-seconds 300

# Require 20 distinct readable text copies while ignoring remote sync churn.
cargo run --bin clipboard_probe -- --expect-text 20 --timeout-seconds 300

# Require 20 distinct text or screenshot copies.
cargo run --bin clipboard_probe -- --expect-items 20 --timeout-seconds 300

Pop-Location
```

Each event is emitted as JSON containing the clipboard sequence number, advertised formats, text or image status, dimensions or length, and a SHA-256 digest. Clipboard contents and image bytes are not printed. Burst mode exits unsuccessfully if it misses a marker, cannot read an update, or reaches the timeout. `--expect-text` counts distinct readable text. `--expect-items` counts distinct readable text or images and ignores non-content synchronization updates.
