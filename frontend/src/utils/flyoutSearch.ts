/**
 * Keys that belong to an IME candidate window, not to type-to-search.
 * Chromium reports the first composing keydown with keyCode 229 before
 * `isComposing` is true; `Process` is the same signal on some layouts.
 */
export function isImeKey(event: { isComposing: boolean; key: string; keyCode?: number }): boolean {
  return event.isComposing || event.key === 'Process' || event.keyCode === 229;
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
