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

A global shortcut registered locally does not fire while a remote-control client
is focused, because the client captures keyboard input first. The same low-level
hook that intercepts `Win+V` also matches the user's configured Cubby hotkey when
a known remote client is the foreground application, and suppresses the chord so
it does not reach the remote session, whatever the user set it to (for example
Ctrl+backtick).

This works only when the remote client's own keyboard forwarding is off. Clients
such as Ninja Remote install their own keyboard capture when forwarding is on,
and because that capture loads after Cubby's it runs ahead of Cubby's hook and
consumes the keys first. With forwarding off, Cubby's hook wins. When forwarding
must stay on, open Cubby from the tray instead.

The trigger is active only for supported remote clients, including Ninja Remote,
Windows Remote Desktop, AnyDesk, TeamViewer, ScreenConnect, Splashtop, and
RustDesk, and only while replacement mode is enabled (that is what runs the
hook). It does nothing in ordinary local applications, where the normal global
shortcut already handles activation.
