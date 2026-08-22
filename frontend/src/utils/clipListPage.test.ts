import { describe, expect, it } from 'vitest';
import { clipListPageArgs, nextPageCursor } from './clipListPage';

describe('clipListPageArgs', () => {
  it('starts at the front on a replace load', () => {
    expect(clipListPageArgs({ loadedCount: 1, cursorId: 'already-on-screen' }, false, 20)).toEqual({
      limit: 20,
      offset: 0,
    });
  });

  it('sends the captured cursor when appending', () => {
    // The backend starts just after this cursor so it does not re-decrypt the
    // 20 (or 2,000) rows already on screen. offset is only the fallback if
    // that id has since disappeared from the filtered set.
    expect(clipListPageArgs({ loadedCount: 2, cursorId: 'last-from-server' }, true, 20)).toEqual({
      limit: 20,
      offset: 2,
      afterId: 'last-from-server',
    });
  });

  it('does not invent a cursor when appending an empty list', () => {
    expect(clipListPageArgs({ loadedCount: 0, cursorId: null }, true, 20)).toEqual({
      limit: 20,
      offset: 0,
    });
  });

  it('falls back to offset when no cursor has been captured yet', () => {
    expect(clipListPageArgs({ loadedCount: 5, cursorId: null }, true, 20)).toEqual({
      limit: 20,
      offset: 5,
    });
  });
});

describe('nextPageCursor', () => {
  it('takes the last row of the page the backend just returned', () => {
    expect(nextPageCursor([{ id: 'a' }, { id: 'b' }], null)).toBe('b');
  });

  it('keeps the previous cursor when a page comes back empty', () => {
    // A short or empty final page must not reset the position to null and
    // send the next append back to offset-only paging.
    expect(nextPageCursor([], 'still-valid')).toBe('still-valid');
  });
});

describe('a local re-sort must not move the paging cursor', () => {
  /**
   * The bug this guards. get_clips/search_clips order by
   * is_pinned DESC, created_at DESC, uuid DESC. A single pin/unpin re-sorts
   * only the loaded window in memory and does not reload, so unpinning an
   * older pinned clip drops it to the end of that array. Deriving afterId
   * from the last array element then resumes the query from deep in the
   * result set, skipping every row between the real window end and that old
   * clip -- and the resulting short page reads as the end of history.
   */
  const serverOrder = [
    { id: 'pinned-old', is_pinned: true, created_at: '2026-01-01' },
    { id: 'newest', is_pinned: false, created_at: '2026-08-20' },
    { id: 'window-end', is_pinned: false, created_at: '2026-08-19' },
  ];

  function resortLikeTogglePin(clips: typeof serverOrder, unpinId: string) {
    return clips
      .map((clip) => (clip.id === unpinId ? { ...clip, is_pinned: false } : clip))
      .sort(
        (left, right) =>
          Number(right.is_pinned) - Number(left.is_pinned) ||
          new Date(right.created_at).getTime() - new Date(left.created_at).getTime()
      );
  }

  it('keeps the server cursor after an unpin sorts an old clip to the bottom', () => {
    const cursor = nextPageCursor(serverOrder, null);
    expect(cursor).toBe('window-end');

    const displayed = resortLikeTogglePin(serverOrder, 'pinned-old');
    // The unpinned clip is now last on screen, which is exactly what the old
    // array-derived cursor would have sent.
    expect(displayed[displayed.length - 1].id).toBe('pinned-old');

    const page = clipListPageArgs({ loadedCount: displayed.length, cursorId: cursor }, true, 20);
    expect(page.afterId).toBe('window-end');
    expect(page.afterId).not.toBe('pinned-old');
  });
});
