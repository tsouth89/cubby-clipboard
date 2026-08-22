import { describe, expect, it } from 'vitest';
import { clipListPageArgs } from './clipListPage';

describe('clipListPageArgs', () => {
  it('starts at the front on a replace load', () => {
    expect(clipListPageArgs([{ id: 'already-on-screen' }], false, 20)).toEqual({
      limit: 20,
      offset: 0,
    });
  });

  it('sends the last visible id when appending', () => {
    // The backend starts just after this cursor so it does not re-decrypt the
    // 20 (or 2,000) rows already on screen. offset is only the fallback if
    // that id has since disappeared from the filtered set.
    expect(clipListPageArgs([{ id: 'first' }, { id: 'last-visible' }], true, 20)).toEqual({
      limit: 20,
      offset: 2,
      afterId: 'last-visible',
    });
  });

  it('does not invent a cursor when appending an empty list', () => {
    expect(clipListPageArgs([], true, 20)).toEqual({ limit: 20, offset: 0 });
  });
});
