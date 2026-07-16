# Shortcut behavior

## Defaults

- New installations use `Win+Alt+V`.
- Win+V replacement is enabled by default and can be disabled in Settings.
- The shortcut is customizable.
- If the configured shortcut is unavailable at startup, Cubby falls back to `Win+Ctrl+Alt+V` and persists the fallback.
- Changing to a conflicting shortcut keeps the previous working shortcut and returns a visible error.

## Win+V

Windows reserves `Win+V` for Clipboard History and rejects attempts to register
it through the normal global-hotkey API. Cubby therefore handles replacement in
an isolated helper mode instead of installing a keyboard hook on the main UI
process.

The helper is a second instance of Cubby's own executable launched before the
normal application startup path. No separate runtime or remapping application is
required. The helper intercepts only an exact physical `Win+V` chord and signals
the main Cubby process through a loopback-only activation channel. It ignores its
own injected events, restores the physical Windows-key state before unrelated
input continues, and exits independently if Cubby disables replacement mode.

Cubby monitors the helper and restarts it after an unexpected exit. The helper
also monitors its parent process, so a Cubby crash cannot leave the hook behind.
Disabling replacement or exiting Cubby restores native Windows Clipboard History.
Cubby remains fully functional with the configured global shortcut when
replacement mode is disabled.

## Remote-session trigger

Some remote-control clients consume Windows-key shortcuts before the host can
handle them. Cubby therefore supports double-tapping either Ctrl key while a
known remote client is the foreground application.

The gesture is active only for supported remote clients, including Ninja Remote,
Windows Remote Desktop, AnyDesk, TeamViewer, ScreenConnect, Splashtop, and
RustDesk. Double-Ctrl does nothing in ordinary local applications.
