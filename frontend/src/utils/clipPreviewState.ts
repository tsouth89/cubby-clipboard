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
 * Edit needs the full payload. A reveal already fetched it, so waiting on
 * `details` would leave the button disabled forever — that fetch is skipped
 * for revealed text so the pane does not go blank.
 */
export function isEditReady(
  details: ClipDetails | null,
  detailsError: string | null,
  revealedContent: string | undefined
): boolean {
  return details !== null || detailsError !== null || Boolean(revealedContent);
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
  return { ...details, ocr_text: text.trim().length > 0 ? text : null };
}
