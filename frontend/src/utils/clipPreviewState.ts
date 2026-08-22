import { applyOcrTextToWords, OcrLayout } from './ocrSelection';

/** Payload of the `get_clip_details` command. */
export interface ClipDetails {
  content: string;
  ocr_text: string | null;
  image_expired: boolean;
  ocr_layout: OcrLayout | null;
  notes: string | null;
}

/** Full text for the editor: details first, then a reveal payload, never the row preview. */
export function fullTextForEdit(
  details: ClipDetails | null,
  revealedContent: string | undefined,
  clipContent: string | undefined
): string {
  return details?.content ?? revealedContent ?? clipContent ?? '';
}

/**
 * Body text for the preview pane: details first, then the reveal payload or the
 * list row, and the stored preview last.
 *
 * Loaded details win outright, empty body included: after the user clears a
 * clip and saves, `content` really is `''`, and falling through to the row
 * would redisplay the pre-save prefix (and its character count) as if the save
 * had not applied.
 *
 * Only once `details` is null do the fallbacks test for *text* rather than for
 * `undefined`. A list row loaded with `previewOnly` carries `content: ''`, so
 * `??` would stop on that empty string and leave the pane blank with "0
 * characters" for as long as the details fetch runs — longest on the large
 * dumps `previewOnly` exists for — and forever if that fetch fails. Showing the
 * stored preview is not a substitute for the body, which is why Edit still
 * refuses to open from it (see `isEditReady`).
 */
export function previewBodyText(
  details: ClipDetails | null,
  sourceContent: string | undefined,
  rowPreview: string | undefined
): string {
  if (details) return details.content;
  return sourceContent || rowPreview || '';
}

/**
 * Edit needs the full payload. A reveal already fetched it, so waiting on
 * `details` would leave the button disabled forever — that fetch is skipped
 * for revealed text so the pane does not go blank. A failed details load is
 * explicitly *not* ready: the only fallback left is the list row's truncated
 * preview, and editing from it would silently chop the clip on save.
 */
export function isEditReady(
  details: ClipDetails | null,
  revealedContent: string | undefined
): boolean {
  return details !== null || revealedContent !== undefined;
}

/** Keep the pane in sync after Save without waiting for a clipId change. */
export function withSavedText(
  details: ClipDetails | null,
  text: string,
  notes: string | null
): ClipDetails {
  return details
    ? { ...details, content: text }
    : {
        content: text,
        ocr_text: null,
        image_expired: false,
        ocr_layout: null,
        notes,
      };
}

/**
 * Keep Scan text and drag-select boxes in sync after Save correction.
 * `has_ocr_text` often does not change, so the pane would otherwise keep the
 * engine's word texts while Copy text already uses the fix (SBS-1010).
 */
export function withSavedOcrText(details: ClipDetails | null, text: string): ClipDetails | null {
  if (!details) return null;
  const normalized = text.trim();
  if (!normalized) {
    return { ...details, ocr_text: null, ocr_layout: null };
  }
  if (!details.ocr_layout) {
    return { ...details, ocr_text: normalized };
  }
  const words = applyOcrTextToWords(details.ocr_layout.words, normalized);
  return {
    ...details,
    ocr_text: normalized,
    ocr_layout: words.length > 0 ? { ...details.ocr_layout, words } : null,
  };
}
