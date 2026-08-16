/**
 * Row-level presentation decisions shared by ClipCard and the image window.
 * Extracted so SBS-807's two History bugs fail in unit tests instead of only
 * in a rendered tree (there is no component-test harness yet).
 */

/**
 * Whether a clip row should show its note.
 *
 * `clipType` is required at the call site so an image row cannot "forget" to
 * ask. It must not hide the note: the old ClipCard only rendered notes in the
 * text branch, so screenshot notes were searchable and visible in the History
 * preview but missing on the row itself (SBS-807).
 *
 * A note of `"0"` is a real note. Hidden rows keep notes off the card until
 * reveal — the hidden payload blanks them.
 */
export function shouldRenderClipRowNote(
  notes: string | null | undefined,
  options: { hidden: boolean; clipType: string }
): boolean {
  if (options.hidden) return false;
  if (notes == null || notes === '') return false;
  void options.clipType;
  return true;
}

/**
 * Copy-image availability for the dedicated image window.
 *
 * - `ready`: full bitmap is present
 * - `expired`: header already says the original is gone; offering Copy image
 *   then failing is the SBS-807 bug. History's preview already disables Copy.
 * - `unknown`: details have not loaded (or failed). That is not "ready" —
 *   enabling the button would collapse "not asked yet" into "copy works".
 */
export type FullImageCopyState = 'ready' | 'expired' | 'unknown';

export function fullImageCopyState(imageExpired: boolean | null | undefined): FullImageCopyState {
  if (imageExpired == null) return 'unknown';
  return imageExpired ? 'expired' : 'ready';
}

export function canCopyFullImage(state: FullImageCopyState): boolean {
  return state === 'ready';
}

export function fullImageCopyTitle(state: FullImageCopyState): string {
  if (state === 'expired') return 'Full image expired; only the thumbnail remains';
  if (state === 'unknown') return 'Copy image is unavailable until the image loads';
  return 'Copy image';
}
