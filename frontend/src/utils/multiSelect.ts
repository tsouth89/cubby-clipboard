/**
 * Row multi-selection for the History window (SOU-583).
 *
 * The gestures are the ones every file list uses, so they need no explanation:
 * plain click on the checkbox toggles one row, Ctrl+click toggles without
 * disturbing the rest, and Shift+click extends from the last row you touched.
 *
 * Kept pure and index-based so the rules can be tested without a DOM: the
 * caller owns the rendered order and passes it in.
 */

export interface SelectionGesture {
  shiftKey: boolean;
  ctrlKey: boolean;
  metaKey: boolean;
}

export interface SelectionState {
  ids: ReadonlySet<string>;
  /** Row the next Shift+click extends from; null once the set is emptied. */
  anchorIndex: number | null;
}

export const EMPTY_SELECTION: SelectionState = { ids: new Set(), anchorIndex: null };

/**
 * Apply a click on `index` to the current selection.
 *
 * `order` is the ids in rendered order, which is what makes a Shift range mean
 * "everything between these two rows on screen" rather than something about
 * insertion order.
 */
export function applySelectionClick(
  state: SelectionState,
  order: readonly string[],
  index: number,
  gesture: SelectionGesture
): SelectionState {
  const id = order[index];
  if (id === undefined) return state;

  // Shift extends from the anchor. With no anchor yet there is nothing to
  // extend from, so it behaves as a plain toggle and sets one.
  if (gesture.shiftKey && state.anchorIndex !== null) {
    const from = Math.min(state.anchorIndex, index);
    const to = Math.max(state.anchorIndex, index);
    const ids = new Set(state.ids);
    for (let cursor = from; cursor <= to; cursor += 1) {
      const rangeId = order[cursor];
      if (rangeId !== undefined) ids.add(rangeId);
    }
    // The anchor stays put, so dragging the shift end around keeps growing and
    // shrinking from the same origin instead of walking away from it.
    return { ids, anchorIndex: state.anchorIndex };
  }

  const ids = new Set(state.ids);
  if (ids.has(id)) {
    ids.delete(id);
    // Deselecting the anchor leaves the next Shift+click without a sensible
    // origin, so re-anchor here rather than extending from a row the user just
    // removed.
    return { ids, anchorIndex: ids.size === 0 ? null : index };
  }
  ids.add(id);
  return { ids, anchorIndex: index };
}

/** Select every currently rendered row, or clear if they are all selected. */
export function toggleSelectAll(state: SelectionState, order: readonly string[]): SelectionState {
  if (order.length > 0 && order.every((id) => state.ids.has(id))) {
    return EMPTY_SELECTION;
  }
  return { ids: new Set(order), anchorIndex: order.length > 0 ? order.length - 1 : null };
}

/**
 * Drop ids that are no longer rendered. Selection is over loaded rows, so a
 * filter change or a delete must not leave the count claiming rows that are
 * gone.
 */
export function pruneSelection(state: SelectionState, order: readonly string[]): SelectionState {
  const present = new Set(order);
  const ids = new Set([...state.ids].filter((id) => present.has(id)));
  if (ids.size === state.ids.size) return state;
  return { ids, anchorIndex: ids.size === 0 ? null : state.anchorIndex };
}

/** Selected ids in rendered order, which is the order bulk actions apply in. */
export function selectedInOrder(state: SelectionState, order: readonly string[]): string[] {
  return order.filter((id) => state.ids.has(id));
}
