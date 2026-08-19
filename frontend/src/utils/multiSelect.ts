/**
 * Row multi-selection for the History window (SOU-583).
 *
 * The gestures are the ones every file list uses, so they need no explanation:
 * plain click on the checkbox toggles one row, Ctrl+click toggles without
 * disturbing the rest, and Shift+click extends from the last row you touched.
 *
 * The Shift origin is a clip id, not a row index. Pinning or deleting remaps
 * indices, and an index-based origin would select rows the user never swept
 * (SBS-1007).
 */

export interface SelectionGesture {
  shiftKey: boolean;
  ctrlKey: boolean;
  metaKey: boolean;
}

export interface SelectionState {
  ids: ReadonlySet<string>;
  /** Clip the next Shift+click extends from; null once the set is emptied. */
  anchorId: string | null;
}

export const EMPTY_SELECTION: SelectionState = { ids: new Set(), anchorId: null };

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

  const anchorIndex = state.anchorId === null ? -1 : order.indexOf(state.anchorId);
  // Shift extends from the anchor. With no anchor yet, or after the anchored
  // clip left the list, there is nothing to extend from, so it behaves as a
  // plain toggle and sets one.
  if (gesture.shiftKey && anchorIndex >= 0) {
    const from = Math.min(anchorIndex, index);
    const to = Math.max(anchorIndex, index);
    const ids = new Set(state.ids);
    for (let cursor = from; cursor <= to; cursor += 1) {
      const rangeId = order[cursor];
      if (rangeId !== undefined) ids.add(rangeId);
    }
    return { ids, anchorId: state.anchorId };
  }

  const ids = new Set(state.ids);
  if (ids.has(id)) {
    ids.delete(id);
    return { ids, anchorId: ids.size === 0 ? null : id };
  }
  ids.add(id);
  return { ids, anchorId: id };
}

/** Select every currently rendered row, or clear if they are all selected. */
export function toggleSelectAll(state: SelectionState, order: readonly string[]): SelectionState {
  if (order.length > 0 && order.every((id) => state.ids.has(id))) {
    return EMPTY_SELECTION;
  }
  return { ids: new Set(order), anchorId: order[order.length - 1] ?? null };
}

/**
 * Drop ids that are no longer rendered. Selection is over loaded rows, so a
 * filter change or a delete must not leave the count claiming rows that are
 * gone. The Shift origin follows the clip, not the slot it used to occupy.
 */
export function pruneSelection(state: SelectionState, order: readonly string[]): SelectionState {
  const present = new Set(order);
  const ids = new Set([...state.ids].filter((id) => present.has(id)));
  const anchorId =
    state.anchorId !== null && present.has(state.anchorId) ? state.anchorId : null;
  if (ids.size === state.ids.size && anchorId === state.anchorId) return state;
  return { ids, anchorId: ids.size === 0 ? null : anchorId };
}

/** Selected ids in rendered order, which is the order bulk actions apply in. */
export function selectedInOrder(state: SelectionState, order: readonly string[]): string[] {
  return order.filter((id) => state.ids.has(id));
}
