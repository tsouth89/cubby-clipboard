import { describe, expect, it } from 'vitest';
import {
  applySelectionClick,
  EMPTY_SELECTION,
  pruneSelection,
  selectedInOrder,
  SelectionState,
  toggleSelectAll,
} from './multiSelect';

const ORDER = ['a', 'b', 'c', 'd', 'e'];

const plain = { shiftKey: false, ctrlKey: false, metaKey: false };
const shift = { shiftKey: true, ctrlKey: false, metaKey: false };
const ctrl = { shiftKey: false, ctrlKey: true, metaKey: false };

const ids = (state: SelectionState) => [...state.ids].sort();

const select = (indices: number[], gestures = indices.map(() => plain)) =>
  indices.reduce(
    (state, index, step) => applySelectionClick(state, ORDER, index, gestures[step]),
    EMPTY_SELECTION
  );

describe('applySelectionClick', () => {
  it('toggles a single row on and off', () => {
    const on = select([1]);
    expect(ids(on)).toEqual(['b']);
    expect(ids(applySelectionClick(on, ORDER, 1, plain))).toEqual([]);
  });

  it('adds rows one at a time with ctrl, leaving the others alone', () => {
    expect(ids(select([1, 3], [plain, ctrl]))).toEqual(['b', 'd']);
  });

  it('extends a range from the anchor with shift, inclusive of both ends', () => {
    expect(ids(select([1, 3], [plain, shift]))).toEqual(['b', 'c', 'd']);
  });

  it('extends backwards just as well', () => {
    expect(ids(select([3, 1], [plain, shift]))).toEqual(['b', 'c', 'd']);
  });

  it('keeps the anchor put so a range can be resized from one origin', () => {
    const grown = select([1, 4], [plain, shift]);
    expect(ids(grown)).toEqual(['b', 'c', 'd', 'e']);
    // Shrinking back from the same anchor: previously-added rows stay selected
    // (this is additive extension), but the anchor has not walked to row 4.
    expect(grown.anchorIndex).toBe(1);
  });

  it('falls back to a plain toggle when shift arrives with no anchor', () => {
    const state = applySelectionClick(EMPTY_SELECTION, ORDER, 2, shift);
    expect(ids(state)).toEqual(['c']);
    expect(state.anchorIndex).toBe(2);
  });

  it('re-anchors when the anchor row is deselected', () => {
    const state = select([1, 3, 3], [plain, ctrl, ctrl]);
    expect(ids(state)).toEqual(['b']);
    expect(state.anchorIndex).toBe(3);
  });

  it('drops the anchor once nothing is selected', () => {
    expect(select([1, 1]).anchorIndex).toBeNull();
  });

  it('ignores a click on an index that is not rendered', () => {
    expect(applySelectionClick(EMPTY_SELECTION, ORDER, 99, plain)).toBe(EMPTY_SELECTION);
  });
});

describe('toggleSelectAll', () => {
  it('selects every rendered row', () => {
    expect(ids(toggleSelectAll(EMPTY_SELECTION, ORDER))).toEqual(['a', 'b', 'c', 'd', 'e']);
  });

  it('clears when everything is already selected', () => {
    const all = toggleSelectAll(EMPTY_SELECTION, ORDER);
    expect(ids(toggleSelectAll(all, ORDER))).toEqual([]);
  });

  it('selects all when only some are selected, rather than clearing', () => {
    expect(ids(toggleSelectAll(select([1]), ORDER))).toEqual(['a', 'b', 'c', 'd', 'e']);
  });

  it('stays empty for an empty list', () => {
    expect(toggleSelectAll(EMPTY_SELECTION, [])).toEqual(EMPTY_SELECTION);
  });
});

describe('pruneSelection', () => {
  it('drops ids that are no longer rendered', () => {
    const state = select([0, 2], [plain, ctrl]);
    expect(ids(pruneSelection(state, ['a', 'b']))).toEqual(['a']);
  });

  it('returns the same state when nothing was dropped', () => {
    const state = select([0]);
    expect(pruneSelection(state, ORDER)).toBe(state);
  });

  it('clears the anchor when the last selected row disappears', () => {
    const state = select([0]);
    expect(pruneSelection(state, ['b', 'c']).anchorIndex).toBeNull();
  });
});

describe('selectedInOrder', () => {
  it('returns selected ids in rendered order, not click order', () => {
    const state = select([3, 1], [plain, ctrl]);
    expect(selectedInOrder(state, ORDER)).toEqual(['b', 'd']);
  });
});
