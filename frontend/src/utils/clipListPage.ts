/**
 * How flyout and History ask for the next list/search page.
 *
 * A displayed-row count as `offset` is exact only when every earlier row
 * decrypted. After a skipped neighbor it points at a row the client already
 * has, so later pages send `afterId` (the last visible row) and the backend
 * starts just after that cursor instead of walking the prefix again (SBS-993).
 * `offset` stays as the fallback source-row hint if that cursor is gone.
 */
export function clipListPageArgs(
  clips: readonly { id: string }[],
  append: boolean,
  pageSize: number
): { limit: number; offset: number; afterId?: string } {
  if (!append || clips.length === 0) {
    return { limit: pageSize, offset: 0 };
  }
  return {
    limit: pageSize,
    offset: clips.length,
    afterId: clips[clips.length - 1]?.id,
  };
}
