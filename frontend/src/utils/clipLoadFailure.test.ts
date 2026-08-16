import { describe, expect, it } from 'vitest';
import { clipLoadFailure } from './clipLoadFailure';

describe('clipLoadFailure', () => {
  it('clears the list when a replace load fails', () => {
    // The rows on screen were fetched for a different search, folder, or type
    // filter. Leaving them up showed stale results as if they still matched.
    expect(clipLoadFailure(false)).toEqual({ clearList: true, notify: false });
  });

  it('keeps the list when an append load fails', () => {
    // Page one is still correct for the active filters. Only pagination broke.
    expect(clipLoadFailure(true)).toEqual({ clearList: false, notify: true });
  });

  it('always tells the user exactly one way', () => {
    for (const append of [true, false]) {
      const { clearList, notify } = clipLoadFailure(append);
      // Clearing reveals ClipList's error panel and retry button, so a cleared
      // list is already a message. Neither would fail silently; both would
      // discard good rows and talk over the panel at the same time.
      expect(clearList !== notify).toBe(true);
    }
  });
});
