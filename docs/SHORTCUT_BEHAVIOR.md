# Shortcut behavior

## Defaults

- New installations use `Alt+V`.
- The shortcut is customizable.
- If the configured shortcut is unavailable at startup, Cubby falls back to `Ctrl+Shift+V` and persists the fallback.
- Changing to a conflicting shortcut keeps the previous working shortcut and returns a visible error.

## Win+V

Windows reserves `Win+V` for Clipboard History and rejects attempts to register it through the normal global-hotkey API. Cubby does not install a low-level keyboard hook because doing so can interfere with ordinary typing and other Windows shortcuts.

Users who intentionally want to replace `Win+V` can map it to Cubby's configured shortcut with a dedicated keyboard-remapping tool. Cubby remains fully functional without that mapping.
