import { OcrLayout } from './ocrSelection';

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
 * Every step tests for *text*, not for `undefined`. A list row loaded with
 * `previewOnly` carries `content: ''`, so `??` would stop on that empty string
 * and leave the pane blank with "0 characters" for as long as the details fetch
 * runs — longest on the large dumps `previewOnly` exists for — and forever if
 * that fetch fails. Showing the stored preview is not a substitute for the
 * body, which is why Edit still refuses to open from it (see `isEditReady`).
 */
export function previewBodyText(
  details: ClipDetails | null,
  sourceContent: string | undefined,
  rowPreview: string | undefined
): string {
  return details?.content || sourceContent || rowPreview || '';
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

/** Keep Scan text in sync after Save correction; `has_ocr_text` often does not change. */
export function withSavedOcrText(details: ClipDetails | null, text: string): ClipDetails | null {
  if (!details) return null;
  const normalized = text.trim();
  return { ...details, ocr_text: normalized || null };
}
