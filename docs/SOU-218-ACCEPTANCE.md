# SOU-218 acceptance matrix

**Issue:** [SOU-218](https://linear.app/southforge-ai/issue/SOU-218/harden-lifecycle-display-and-recovery-behavior)  
**Goal:** Cubby must not silently stop capturing; recovery and diagnostics must stay content-free.

Status key: **Pass** (automated or verified) · **Pending** (needs human session) · **Out of scope** (explicitly deferred)

## Shipped implementation

| Slice | Evidence | Status |
|-------|----------|--------|
| Supervised clipboard listener + restart forever | PR #74, `clipboard.rs` | Pass |
| Sequence watchdog (2s poll, 5s stale) | PR #74 | Pass |
| `get_clipboard_capture_status` (no payloads) | PR #74, command registered in `lib.rs` | Pass |
| Startup `PRAGMA quick_check` | PR #113, `database.rs` | Pass |
| Quarantine corrupt DB (+ WAL/SHM) and start fresh | PR #113 + unit tests | Pass |
| Rolling `cubby.db.bak` (max once / 24h) | PR #113 + unit tests | Pass |
| Content-free recovery diagnostics | PR #113 unit test on sanitize | Pass |

## Automated / code-review acceptance

| Check | How verified | Status |
|-------|--------------|--------|
| Missing DB is created on first open | `missing_database_file_is_ready_for_create` | Pass |
| Healthy DB gets rolling backup | `healthy_database_gets_a_rolling_backup` | Pass |
| Fresh backup is not rewritten | `fresh_rolling_backup_is_not_rewritten` | Pass |
| Garbage DB is quarantined; fresh open works | `garbage_database_is_quarantined_and_fresh_open_succeeds` | Pass |
| Structural corruption is quarantined | `structurally_corrupt_sqlite_is_quarantined` | Pass |
| `storage.key` survives quarantine | `quarantine_rename_preserves_sibling_key_file` | Pass |
| Encrypted DB without key still fails closed | `encrypted_database_without_its_key_fails_closed` | Pass |
| Single-instance plugin shows existing window | Code: `tauri_plugin_single_instance` → `position_window_near_cursor` in `lib.rs` | Pass (code review) |
| Portable builds skip autostart registry | Code: portable path in `settings_commands.rs` | Pass (code review) |
| Flyout clamps to monitor work area | Code: `position_window_near_cursor` / work-area clamp in `lib.rs` | Pass (code review) |

## Human session acceptance (still required to close SOU-218)

Run on a daily-driver or clean Windows 11 VM. Do not paste clipboard contents into notes or issues.

### Database (manual smoke after PR #113 merges)

| # | Step | Status |
|---|------|--------|
| D1 | First launch creates `cubby.db`; copy appears in history | Pending |
| D2 | After history exists, `cubby.db.bak` present (or skipped if &lt;24h old) | Pending |
| D3 | Quit; replace `cubby.db` with garbage; relaunch → `*.corrupt-*`, empty history, capture works, `storage.key` remains | Pending |
| D4 | App logs show quarantine/structural text only (no clip bodies) | Pending |

### Single-instance and autostart

| # | Step | Status |
|---|------|--------|
| S1 | Second process while Cubby runs: one process, flyout near cursor | Pending |
| S2 | Installed build: enable autostart, reboot/sign-in, tray + capture | Pending |
| S3 | Portable build: no Run-key autostart writes | Pending (code review Pass; optional re-check on portable artifact) |

### Display and session

| # | Step | Status |
|---|------|--------|
| L1 | Sleep/resume; copy within ~10s is captured | Pending |
| L2 | Lock/unlock; capture still works | Pending |
| L3 | DPI 100%↔150% or unplug monitor; next hotkey positions without crash | Pending |
| L4 | Move taskbar; flyout clamps to work area | Pending |
| L5 | Optional: virtual desktop switch + hotkey on current desktop | Pending |

### Capture health

| # | Step | Status |
|---|------|--------|
| H1 | Invoke `get_clipboard_capture_status`; fields present, no payload | Pending (dev build) |

## Explicitly deferred (not blocking SOU-218 close)

| Item | Why |
|------|-----|
| Explicit power/session Win32 callbacks | Watchdog covers common silent-death cases; add only if L1/L2 fail in the field |
| Settings/About capture-health UI | API is enough for diagnostics; UI is product polish |

When D1–D4, S1–S2, L1–L4, and H1 are marked Pass (or waived with reason), SOU-218 can move to Done.

## How to record results

Edit this file (or leave a Linear comment on SOU-218) with date, machine, and Pass/Fail per row. Keep notes free of clipboard content.
