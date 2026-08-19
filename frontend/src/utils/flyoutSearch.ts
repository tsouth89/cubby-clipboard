/**
 * Keys that belong to an IME candidate window, not to type-to-search.
 * Chromium reports the first composing keydown with keyCode 229 before
 * `isComposing` is true; `Process` is the same signal on some layouts.
 */
export function isImeKey(event: { isComposing: boolean; key: string; keyCode?: number }): boolean {
  return event.isComposing || event.key === 'Process' || event.keyCode === 229;
}

/**
 * Whether a window-level shortcut handler should stand down for this event.
 *
 * Composition belongs to the IME candidate window. A modal confirm owns the
 * keyboard outright: ConfirmDialog listens on `window`, which runs after
 * `document`, so a document handler that keeps acting beats the dialog and
 * answers the question on the user's behalf -- Delete hard-deleting the
 * previewed clip while the bulk confirm is still up (SBS-1007).
 */
export function shortcutsSuspended(
  event: { isComposing: boolean; key: string; keyCode?: number },
  modalOpen: boolean
): boolean {
  return isImeKey(event) || modalOpen;
}

/** Printable keys that should steal focus into the flyout search box. */
export function shouldCaptureTypeToSearch(event: {
  isComposing: boolean;
  key: string;
  keyCode?: number;
  ctrlKey: boolean;
  altKey: boolean;
  metaKey: boolean;
}): boolean {
  if (isImeKey(event)) return false;
  if (event.ctrlKey || event.altKey || event.metaKey) return false;
  return event.key.length === 1;
}
