/** What a failed clip load should do to the rows already on screen. */
export interface ClipLoadFailure {
  /**
   * Drop the visible rows. A replace load owns the whole view, so after it
   * fails the rows on screen no longer describe the active search, folder, or
   * type filter. Keeping them presented stale results as if they matched, and
   * the error panel never appeared because it only renders on an empty list.
   */
  clearList: boolean;
  /**
   * Say the load failed out of band. An append failure keeps a correct first
   * page, so clearing it would throw away good rows; the user still needs to
   * know pagination stopped rather than reaching the end of their history.
   */
  notify: boolean;
}

/**
 * Exactly one of these is always true: either the list clears and the error
 * panel takes over, or a message fires. A failure that does neither is the
 * silent-stale-list bug, and one that does both both discards good rows and
 * talks over its own error panel.
 */
export function clipLoadFailure(append: boolean): ClipLoadFailure {
  return append ? { clearList: false, notify: true } : { clearList: true, notify: false };
}
