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
   * Say the load failed out of band. Used for pagination: the first page is
   * still correct, the user is at the bottom, and a top-of-list banner would
   * sit off screen.
   */
  notify: boolean;
  /**
   * Keep the rows but mark the list stale. ClipList only had an error panel
   * for an empty list (SBS-805); a same-filter refresh that failed used to
   * look like a healthy current page once the toast dismissed.
   */
  showBanner: boolean;
}

/**
 * The user always learns exactly once: empty list → error panel, append →
 * toast, same-filter replace → banner. Doing none is the silent-stale-list
 * bug. Doing two talks over the panel.
 */
export function clipLoadFailure({
  append,
  visibleRowsStillApply,
  hasVisibleClips,
}: ClipLoadAttempt): ClipLoadFailure {
  const clearList = !append && !visibleRowsStillApply;
  const listEmptyAfter = clearList || !hasVisibleClips;
  return {
    clearList,
    notify: !listEmptyAfter && append,
    showBanner: !listEmptyAfter && !append,
  };
}

/**
 * How ClipList should present `loadError`. A populated list used to ignore
 * the flag, which is the "hides the error" half of SBS-805.
 */
export function clipListErrorSurface(
  loadError: boolean,
  clipCount: number
): 'none' | 'panel' | 'banner' {
  if (!loadError) return 'none';
  return clipCount === 0 ? 'panel' : 'banner';
}

/**
 * A load that finished, a load that failed, and a load that no longer owns
 * the view are three states. Callers that treat superseded as success
 * announce work the user cannot see (SBS-805).
 */
export type ClipLoadResult = 'applied' | 'failed' | 'superseded';

export function clipLoadAnnouncesSuccess(result: ClipLoadResult): boolean {
  return result === 'applied';
}

/**
 * Folders, source-app filters, and the history count are sidecar lists.
 * A failed reload must not look like a fresh read of those lists.
 */
export function sidecarReloadFailure(): { keepPrevious: true; notify: true } {
  return { keepPrevious: true, notify: true };
}
