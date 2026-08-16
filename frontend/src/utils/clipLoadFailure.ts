/** The load whose failure is being classified. */
export interface ClipLoadAttempt {
  /** This load adds a page to the rows on screen instead of replacing them. */
  append: boolean;
  /**
   * The rows on screen still answer this load's query. A clipboard-change
   * refresh, a window-focus refresh, a post-edit reload, and the retry button
   * all re-run the query the visible rows already answer, so they keep it true.
   * A filter change makes those rows describe something the user is no longer
   * looking at, and so does a delete or a clear — which is why those callers
   * mark the rows stale before reloading.
   */
  visibleRowsStillApply: boolean;
  /** There are rows on screen right now. */
  hasVisibleClips: boolean;
}

/** What a failed clip load should do to the rows already on screen. */
export interface ClipLoadFailure {
  /**
   * Drop the visible rows. Only a replace whose rows have stopped applying owns
   * rows that no longer describe the view; keeping those presents stale results
   * as if they matched, and the error panel never appears because it renders on
   * an empty list. A replace that re-runs the query the rows already answer
   * would instead trade a correct page for an empty error panel.
   */
  clearList: boolean;
  /**
   * Say the load failed out of band. The error panel only renders on an empty
   * list, so any failure that leaves rows on screen is otherwise silent.
   */
  notify: boolean;
}

/**
 * The user always learns exactly once: either the list ends up empty and the
 * error panel takes over, or a message fires. Doing neither is the
 * silent-stale-list bug, and doing both talks over the panel.
 */
export function clipLoadFailure({
  append,
  visibleRowsStillApply,
  hasVisibleClips,
}: ClipLoadAttempt): ClipLoadFailure {
  const clearList = !append && !visibleRowsStillApply;
  return { clearList, notify: !clearList && hasVisibleClips };
}
