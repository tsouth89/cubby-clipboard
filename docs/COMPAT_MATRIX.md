# Clipboard application-compatibility matrix

SBS-408. Where `scripts/test-clipboard-formats.ps1` proves the *clipboard* can
carry Cubby's payloads, this matrix proves real *applications* can receive them.
Each row drives one class of paste target end to end and reports failures with
the app, format, and step that produced them.

```powershell
scripts/test-compat-matrix.ps1                 # every class
scripts/test-compat-matrix.ps1 -Only browser   # one class
scripts/test-compat-matrix.ps1 -List           # available class names
scripts/test-compat-matrix.ps1 -KeepApps       # leave apps open to inspect a failure
```

The suite drives the real desktop: it takes the foreground, opens and closes
applications, and replaces the clipboard. Do not use the machine while it runs.

## Steps

Every row walks the same three steps, and a failure names the one it stopped in.

| Step | What is asserted |
| --- | --- |
| `capture` | Cubby's shared capture policy classifies the payload correctly -- storable text is storable, a bare file payload is ignored. |
| `restore` | Writing the payload to the clipboard and reading it back returns identical bytes. |
| `paste` | The real paste engine (`cubby::paste_engine`) drives the target application, and the payload is read back **out of that application**. |

Durable-storage assertions are not part of a row. They are covered by the
database and clipboard test suites in `cargo test`; this matrix deliberately
stops at the clipboard boundary rather than standing up an app instance.

## Rows

| Class | Target | Notes |
| --- | --- | --- |
| `legacy_win32` | An `EDIT` control the harness owns | No prerequisites. Isolates the paste engine from application quirks. |
| `packaged_app` | Notepad (Store-packaged on Windows 11) | Launched through the `notepad.exe` stub, which hands off to another process. |
| `explorer` | File Explorer | The file-payload row: see below. |
| `terminal` | PowerShell in Windows Terminal | Needs `wt.exe`. |
| `browser` | Edge, then Chrome | Runs against a generated local page with a labelled textarea. Chromium-based only; see below. |
| `ide` | VS Code, then Notepad++ | Skips when neither exposes an automatable control. |
| `office` | Word, then WordPad | Opens an RTF document. |
| `remote_session` | The owned `EDIT` control, driven with `PasteStrategy::RemoteSession` | Exercises the remote-session timing path on any machine. |
| `elevated` | The owned `EDIT` control at high integrity | Only runs when the suite itself is elevated. |

The `explorer` row is the one that asserts a *product contract* rather than a
round trip. Cubby does not store file payloads, so the row checks that the
payload is classified as ignored, that Explorer still pastes the file (verified
on disk), and that a following text copy is still captured -- an ignored payload
must not wedge capture.

## Skips are not passes

A row whose application is missing, or whose environment does not apply, is
reported as `skipped` with a reason, and its class is listed in
`classes_not_covered`. A run with no failures and six skips is green but proves
very little, so the script prints the uncovered classes as a warning. Read that
line before treating a green run as coverage.

Skip reasons in practice:

- **no candidate application installed** -- install one of the listed apps.
- **exposed no UI Automation text control** -- the app is installed but cannot
  be driven. VS Code does this: its launcher exits immediately and the Electron
  window exposes no text control to UI Automation.
- **this run is not elevated** -- start an elevated shell and re-run. UIPI
  blocks synthetic paste into an elevated window from a medium-integrity
  process by design, so this class can only be demonstrated from elevation.

## Things that are load-bearing

These were all found the hard way; changing them will silently weaken the suite.

- **The browser row targets a labelled textarea, not "the first editable
  control".** A browser window exposes its address bar in the same automation
  tree, and it sorts first. Pasting into the omnibox reads the payload back
  perfectly and looks like a pass while never involving the page.
- **The browser needs its own `--user-data-dir`.** An already-running browser
  adopts the URL into its existing process and drops the rest of the command
  line, which would leave the row driving an unrelated window.
- **Chromium needs `--force-renderer-accessibility`.** Page content is absent
  from the automation tree without it.
- **The browser row is Chromium-only, deliberately.** `--user-data-dir` and
  `--force-renderer-accessibility` are Chromium flags. Firefox uses `-profile`
  and initialises accessibility on demand, so handing it this argument list
  would let an existing Firefox session take the URL and leave the row driving
  the wrong window. Adding Firefox means giving it its own arguments and
  verifying them, not appending to the candidate list.
- **Activate, then focus, then paste.** Activating a window can move focus
  inside it, so focusing a control first is silently undone.
- **The terminal row uses PowerShell, not `cmd.exe`.** `cmd`'s `set /p` reads
  console input through the console code page and returns `caf?` for `café`
  even after `chcp 65001`. That is a cmd limitation, and using it would report
  a Cubby paste bug that does not exist.
- **The terminal row uses `wt -w new`.** Spawning a console app with
  `CREATE_NEW_CONSOLE` opens a *tab* in the existing Windows Terminal window on
  Windows 11, leaving no window of our own to paste into; routing through
  `conhost.exe` instead makes conhost take over the harness's own console.
- **Commands passed to a shell contain no double quotes.** Rust escapes inner
  quotes as `\"` when building a command line, which neither cmd nor PowerShell
  parses as intended.
- **Word's content is only readable through the Text pattern.** Its document
  also exposes a Value pattern, which always reads back empty.

## Cleanup

The suite leaves the desktop as it found it: window count is unchanged across
repeated runs. Getting there takes more than killing the process it spawned,
because the two obvious approaches each fail on half the rows.

- **Closing the window is not enough.** An editor holding pasted text in an
  unsaved document answers `WM_CLOSE` with a save prompt and stays on screen.
- **Killing the spawned child is not enough.** Windows 11 Notepad and every
  browser hand the launch off to another process and exit, so the child is
  already gone.

So each row closes its window and then terminates only processes of that
executable that did not exist before the row started. This happens in the
launched application's `Drop`, not at the end of the driving function: a row
that fails early -- no window appeared, no automatable control -- is the one
most likely to have left something on screen, and cleanup written after the
last `?` would skip exactly those cases.

Two rows deliberately opt out of the terminate half:

- **Browsers.** A browser runs a process per renderer and spawns them
  constantly, so killing every `msedge.exe` that appeared during the run would
  take the user's own tabs with it. A throwaway profile with a single window
  exits cleanly on `WM_CLOSE` instead.
- **Explorer and Windows Terminal.** Their windows belong to long-running host
  processes -- the shell, and a `WindowsTerminal.exe` that may own the user's
  other terminals -- so these rows close the window and never the process.

Residual case: if the target application was **already running** when the suite
started, a row may open a document in that existing process rather than a new
one. Nothing new is terminated, and that window can be left behind with a save
prompt. Close the target apps before a full run if that matters.

`-KeepApps` disables all of this, which is what makes it useful for inspecting
a failure.

## Interpreting a failure

`office / winword.exe / unicode_text / paste` means Word was found and driven,
the clipboard round trip succeeded, and the payload did not arrive in the
document. Re-run that row alone with `-Only office -KeepApps` and inspect the
window that is left open.

UI automation on a busy desktop is not perfectly deterministic: each row retries
the activate/focus/paste sequence three times before reporting a failure. A
single red row that passes on a focused re-run is worth re-running once before
being treated as a regression.
