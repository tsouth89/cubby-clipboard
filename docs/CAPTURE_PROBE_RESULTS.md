# Native capture probe results

Test date: 2026-07-16

Environment: Windows 11 development machine

## Local text burst

The probe listens through `AddClipboardFormatListener` and processes `WM_CLIPBOARDUPDATE`. A separate child process writes a unique marker for every copy so clipboard ownership matches normal cross-process application behavior.

| Copies | Interval | Notifications | Materialized markers | Read failures | Result |
| ---: | ---: | ---: | ---: | ---: | --- |
| 100 | 25 ms | 100 | 100 | 0 | Pass |
| 100 | 10 ms | 100 | 100 | 0 | Pass |

Commands:

```powershell
cargo run --locked --bin clipboard_probe -- --burst 100 --interval-ms 25 --timeout-seconds 15
cargo run --locked --bin clipboard_probe -- --burst 100 --interval-ms 10 --timeout-seconds 15
```

## Findings

- The native Windows notification path delivered one observable update for every test copy.
- Unicode text could be materialized for every marker without waiting 150 ms or discarding earlier events.
- Clipboard format enumeration was sometimes transiently empty even though Unicode text was immediately readable. Capture must attempt important known formats directly and treat enumeration as useful metadata, not the only authority for whether content exists.
- A same-process writer can expose intermediate clipboard state between clearing and setting data. The harness therefore uses a separate writer process to model real application ownership.

## Remaining proof

- Run the interactive probe during RDP and NinjaOne sessions.
- Add HTML, RTF, image, physical file-list, and virtual-file burst fixtures.
- Verify content remains available after disconnecting or closing the source session.
- Move the proven event and retry behavior into Cubby's production capture engine.

For a remote text run, use:

```powershell
cargo run --locked --bin clipboard_probe -- --expect-text 20 --timeout-seconds 300
```

Copy 20 distinct text values in the remote session. Remote-control software may emit additional updates with no readable text; those are reported as synchronization churn and do not count toward the target.

For a mixed text and screenshot run, use:

```powershell
cargo run --locked --bin clipboard_probe -- --expect-items 20 --timeout-seconds 300
```

This mode materializes and hashes screenshots as images, counts each distinct text or image payload once, and ignores duplicate synchronization notifications.
