/**
 * How flyout and History ask for the next list/search page.
 *
 * A displayed-row count as `offset` is exact only when every earlier row
 * decrypted. After a skipped neighbor it points at a row the client already
 * has, so later pages send `afterId` (a cursor) and the backend starts just
 * after it instead of walking the prefix again (SBS-993). `offset` stays as
 * the fallback source-row hint if that cursor is gone.
 *
 * The cursor is supplied rather than read off the end of the displayed array,
 * because the array is not always in server order. A single pin/unpin re-sorts
 * the loaded window in place, which can move an older clip to the end; using
 * that row as the cursor would tell the backend to resume from deep in the
 * result set and silently skip every row in between.
 */
export function clipListPageArgs(
  page: { loadedCount: number; cursorId: string | null },
  append: boolean,
  pageSize: number
): { limit: number; offset: number; afterId?: string } {
  if (!append || page.loadedCount === 0) {
    return { limit: pageSize, offset: 0 };
  }
  return {
    limit: pageSize,
    offset: page.loadedCount,
    afterId: page.cursorId ?? undefined,
  };
}

/**
 * The cursor for the page after `received`, which arrived from the backend in
 * server order. Returns the previous cursor when a page comes back empty so a
 * short final page does not discard a still-valid position.
 */
export function nextPageCursor(
  received: readonly { id: string }[],
  previous: string | null
): string | null {
  return received.length > 0 ? received[received.length - 1].id : previous;
}
