# Shortcut acceptance

Cubby's shortcut behavior is not complete until this matrix passes on Windows 11.

## Cubby-native shortcut

- `Win+Alt+V` opens Cubby when it is hidden.
- Pressing `Win+Alt+V` again closes Cubby, even if the overlay lost focus.
- Holding the modifiers and repeatedly pressing `V` alternates open and closed without key-state damage.
- Ordinary `V` typing works immediately before and after invoking Cubby.
- `Alt+V` remains available to application menus.
- `Win+V`, `Win+Period`, `Win+E`, `Win+R`, and a plain Windows-key tap retain their normal Windows behavior.
- A conflicting configured shortcut produces a visible error and keeps the previous working shortcut.
- Restart, sleep/resume, lock/unlock, Ninja Remote, and RDP do not disable the shortcut.

## Win+V replacement

This default mode is provided by Cubby's bundled Windows shortcut helper. Users do not
need PowerToys or another remapping application.

The helper intercepts an exact physical `Win+V` chord and forwards it to Cubby's
privately registered `Win+Alt+V` shortcut. Additional held modifiers do not
trigger Cubby. If the helper is stopped or crashes, its keyboard hook disappears
and native Windows behavior is restored.

Acceptance:

- The first `Win+V` opens Cubby.
- The second `Win+V` closes Cubby.
- Holding `Win` and repeatedly tapping `V` continues toggling Cubby.
- Windows Clipboard History never appears behind Cubby.
- Repeated presses remain reliable.
- Plain `V` works immediately before and after the replacement shortcut.
- `Win+E`, `Win+R`, `Win+Period`, and a plain Windows-key tap remain unchanged.
- Additional modifiers such as `Ctrl+Win+V` do not trigger Cubby.
- Ordinary typing and all unrelated Windows shortcuts remain unchanged.
- Stopping the helper restores native Windows Clipboard History without
  restarting Cubby.
- Helper startup, shutdown, partial input injection, and Cubby shutdown do not
  leave any modifier or letter key logically held.
- If the helper exits unexpectedly, Cubby restarts it.
- If Cubby exits unexpectedly, the helper detects the parent exit and stops.
