import { describe, expect, it } from 'vitest';
import { clipLoadFailure } from './clipLoadFailure';

describe('clipLoadFailure', () => {
  it('clears the list when a replace load for new filters fails', () => {
    // The rows on screen were fetched for a different search, folder, or type
    // filter. Leaving them up shows stale results as if they still matched.
    expect(
      clipLoadFailure({ append: false, visibleRowsStillApply: false, hasVisibleClips: true })
    ).toEqual({ clearList: true, notify: false });
  });

  it('keeps the list when a refresh with unchanged filters fails', () => {
    // Clipboard-change, window focus, post-edit, and pin reloads all replace
    // the list with the same query. The rows are still correct, so a failed
    // refresh must not swap a good first page for the empty error panel.
    expect(
      clipLoadFailure({ append: false, visibleRowsStillApply: true, hasVisibleClips: true })
    ).toEqual({ clearList: false, notify: true });
  });

  it('does not clear the list when a pin reload fails', () => {
    // Pinning does not remove rows from the current query. afterBulkChange
    // leaves visibleRowsStillApply true so a failed replace keeps them.
    expect(
      clipLoadFailure({ append: false, visibleRowsStillApply: true, hasVisibleClips: true })
        .clearList
    ).toBe(false);
  });

  it('keeps the list when an append load fails', () => {
    // Page one is still correct for the active filters. Only pagination broke.
    expect(
      clipLoadFailure({ append: true, visibleRowsStillApply: true, hasVisibleClips: true })
    ).toEqual({ clearList: false, notify: true });
  });

  it('stays quiet when nothing was on screen to keep', () => {
    // A first load, or a retry after the list already emptied, leaves the error
    // panel showing. A message on top of it would say the same thing twice.
    expect(
      clipLoadFailure({ append: false, visibleRowsStillApply: true, hasVisibleClips: false })
    ).toEqual({ clearList: false, notify: false });
  });

  it('always tells the user exactly one way', () => {
    for (const append of [true, false]) {
      for (const visibleRowsStillApply of [true, false]) {
        for (const hasVisibleClips of [true, false]) {
          const { clearList, notify } = clipLoadFailure({
            append,
            visibleRowsStillApply,
            hasVisibleClips,
          });
          // ClipList renders its error panel and retry button only on an empty
          // list, so an empty list is already a message. Neither would fail
          // silently; both would talk over the panel.
          const listEmptyAfter = clearList || !hasVisibleClips;
          expect(listEmptyAfter !== notify).toBe(true);
        }
      }
    }
  });
});
