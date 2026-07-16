# Shortcut behavior

## Defaults

- New installations use `Win+Shift+V`.
- The shortcut is customizable.
- If the configured shortcut is unavailable at startup, Cubby falls back to `Ctrl+Shift+V` and persists the fallback.
- Changing to a conflicting shortcut keeps the previous working shortcut and returns a visible error.

## Replace Win+V

`Replace Windows Clipboard` is an experimental, opt-in setting.

When enabled, Cubby installs a low-level Windows keyboard hook that:

- detects `Win+V`;
- suppresses the original Windows Clipboard History invocation;
- opens or closes Cubby through the same popup state machine as the standard shortcut;
- leaves `Win+Period` untouched for emojis, GIFs, kaomoji, and symbols;
- does no work beyond updating key state and signaling a worker thread from inside the hook callback.

The standard configurable shortcut remains active as a fallback. Disabling the setting immediately stops interception without restarting Cubby.

## Operational warning

Windows reserves shortcuts involving the Windows key, so `Win+V` cannot be claimed through the normal global-hotkey API. The replacement uses the supported `WH_KEYBOARD_LL` hook mechanism but remains experimental until it has broader application, security-tool, sleep/resume, and long-running reliability coverage.
