# Remote-session behavior

Remote-control applications handle keyboard and clipboard input differently from
ordinary Windows applications. Cubby detects supported remote clients when the
flyout opens and selects a compatible paste strategy.

## Ninja Remote

Recommended player settings:

- Enable clipboard synchronization.
- Keep Forward keyboard shortcuts enabled if that is your normal workflow.

Recommended Cubby setting:

- Remote session paste: **Copy, then Ctrl+V**.

Workflow:

1. Focus the destination inside Ninja Remote.
2. Double-tap Left Ctrl or Right Ctrl.
3. Select a clip in Cubby.
4. Cubby writes the complete item to the local clipboard, closes, restores focus
   to Ninja.
5. Press physical Ctrl+V.

This is the preferred mode for large logs, scripts, formatted text, and other
large clipboard items. Ninja synchronizes the clipboard as data, so content is
not typed character by character.

The optional **Paste as keystrokes** mode invokes Ninja's own toolbar action
through Windows accessibility. It is useful for short text or sensitive values,
but Ninja deliberately types the content into the remote session and is
therefore unsuitable for large items.

Cubby does not synthesize Ctrl+V for Ninja. Ninja's player accepts physical
keyboard input for forwarding but ignores the synthetic input methods available
to ordinary Windows applications.

## Other remote clients

Cubby recognizes Windows Remote Desktop, AnyDesk, TeamViewer, ScreenConnect,
Splashtop, and RustDesk. These clients currently use focus restoration followed
by Cubby's standard Ctrl+V compatibility path. Each client should be validated
individually because forwarding and clipboard policies vary.

## Testing

- `cargo test --all-targets` covers remote-client detection, trigger state, and
  paste-mode selection.
- `scripts/test-remote-trigger.ps1` validates the helper's direct activation
  channel and double-Ctrl gesture without requiring Ninja.
- `scripts/test-paste-compat.ps1` exercises standard and generic remote focus and
  paste behavior in an isolated Windows test window.
