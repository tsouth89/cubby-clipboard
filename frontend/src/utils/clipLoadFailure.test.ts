import { describe, expect, it } from 'vitest';
import {
  clipListErrorSurface,
  clipLoadAnnouncesSuccess,
  clipLoadFailure,
  sidecarReloadFailure,
} from './clipLoadFailure';

describe('clipLoadFailure', () => {
  it('clears the list when a replace load for new filters fails', () => {
    // The rows on screen were fetched for a different search, folder, or type
    // filter. Leaving them up shows stale results as if they still matched.
    expect(
      clipLoadFailure({ append: false, visibleRowsStillApply: false, hasVisibleClips: true })
    ).toEqual({ clearList: true, notify: false, showBanner: false });
  });

  it('keeps the list and shows a banner when a refresh with unchanged filters fails', () => {
    // Clipboard-change, window focus, post-edit, and pin reloads all replace
    // the list with the same query. The rows are still correct, so a failed
    // refresh must not swap a good first page for the empty error panel —
    // but it also must not look healthy. The banner is that third state.
    expect(
      clipLoadFailure({ append: false, visibleRowsStillApply: true, hasVisibleClips: true })
    ).toEqual({ clearList: false, notify: false, showBanner: true });
  });

  it('does not clear the list when a pin reload fails', () => {
    // Pinning does not remove rows from the current query. afterBulkChange
    // leaves visibleRowsStillApply true so a failed replace keeps them.
    expect(
      clipLoadFailure({ append: false, visibleRowsStillApply: true, hasVisibleClips: true })
        .clearList
    ).toBe(false);
  });

  it('keeps the list and toasts when an append load fails', () => {
    // Page one is still correct for the active filters. Only pagination broke.
    // The user is at the bottom, so a top banner would sit off screen.
    expect(
      clipLoadFailure({ append: true, visibleRowsStillApply: true, hasVisibleClips: true })
    ).toEqual({ clearList: false, notify: true, showBanner: false });
  });

  it('stays quiet when nothing was on screen to keep', () => {
    // A first load, or a retry after the list already emptied, leaves the error
    // panel showing. A message or banner on top of it would say the same thing twice.
    expect(
      clipLoadFailure({ append: false, visibleRowsStillApply: true, hasVisibleClips: false })
    ).toEqual({ clearList: false, notify: false, showBanner: false });
  });

  it('always tells the user exactly one way', () => {
    for (const append of [true, false]) {
      for (const visibleRowsStillApply of [true, false]) {
        for (const hasVisibleClips of [true, false]) {
          const { clearList, notify, showBanner } = clipLoadFailure({
            append,
            visibleRowsStillApply,
            hasVisibleClips,
          });
          // ClipList renders its error panel on an empty list and a stale
          // banner on a populated one. Neither plus a toast would talk over
          // the in-list UI; both missing is the silent-stale-list bug.
          const listEmptyAfter = clearList || !hasVisibleClips;
          expect(Number(listEmptyAfter) + Number(notify) + Number(showBanner)).toBe(1);
        }
      }
    }
  });
});

describe('clipListErrorSurface', () => {
  it('hides nothing when there is no load error', () => {
    expect(clipListErrorSurface(false, 0)).toBe('none');
    expect(clipListErrorSurface(false, 3)).toBe('none');
  });

  it('uses the empty-list panel when the list is empty', () => {
    expect(clipListErrorSurface(true, 0)).toBe('panel');
  });

  it('uses a banner when a failed reload left rows on screen', () => {
    // SBS-805: ClipList used to return the healthy list whenever
    // clips.length > 0, so loadError was set and then ignored.
    expect(clipListErrorSurface(true, 3)).toBe('banner');
  });
});

describe('clipLoadAnnouncesSuccess', () => {
  it('announces only a load that actually landed', () => {
    expect(clipLoadAnnouncesSuccess('applied')).toBe(true);
    expect(clipLoadAnnouncesSuccess('failed')).toBe(false);
    // Superseded is unknown, not success. Treating it as true made a
    // delete/unhide toast fire while the newer load still owned the view.
    expect(clipLoadAnnouncesSuccess('superseded')).toBe(false);
  });
});

describe('sidecarReloadFailure', () => {
  it('keeps the last-known-good sidecar list and requires a notify', () => {
    // Folders, source apps, and the history count used to catch, log, and
    // leave the previous value up with no user-visible error.
    expect(sidecarReloadFailure()).toEqual({ keepPrevious: true, notify: true });
  });
});
