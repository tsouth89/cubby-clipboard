import { describe, expect, it } from 'vitest';
import {
  ClipDetails,
  fullTextForEdit,
  isEditReady,
  previewBodyText,
  withSavedOcrText,
  withSavedText,
} from './clipPreviewState';

const details: ClipDetails = {
  content: 'old body',
  ocr_text: 'old scan',
  image_expired: false,
  ocr_layout: null,
  notes: 'keep me',
};

describe('fullTextForEdit', () => {
  it('prefers loaded details over the reveal copy and the list row', () => {
    expect(fullTextForEdit(details, 'revealed', 'row')).toBe('old body');
  });

  it('uses the reveal payload when details were never fetched', () => {
    expect(fullTextForEdit(null, 'revealed body', '')).toBe('revealed body');
  });

  it('falls back to the list row only when nothing else is loaded', () => {
    expect(fullTextForEdit(null, undefined, 'row body')).toBe('row body');
  });
});

describe('previewBodyText', () => {
  it('shows the stored preview while details are still loading a preview_only row', () => {
    // The row as `get_clips` with `previewOnly: true` ships it.
    expect(previewBodyText(null, '', 'copied log line')).toBe('copied log line');
  });

  it('still shows the stored preview when the details load fails', () => {
    // Same inputs — a failed fetch leaves `details` null — so the banner about
    // showing the list preview is not sitting over a blank body.
    expect(previewBodyText(null, '', 'copied log line')).toBe('copied log line');
  });

  it('replaces the preview with the full body once details land', () => {
    expect(previewBodyText(details, '', 'copied log line')).toBe('old body');
  });

  it('prefers a reveal payload over the row preview', () => {
    expect(previewBodyText(null, 'revealed body', 'copied log line')).toBe('revealed body');
  });

  it('uses a full list row when preview_only was not requested', () => {
    expect(previewBodyText(null, 'the whole clip', 'the whole')).toBe('the whole clip');
  });

  it('renders nothing when there is nothing at all, rather than "undefined"', () => {
    expect(previewBodyText(null, '', '')).toBe('');
    expect(previewBodyText(null, undefined, undefined)).toBe('');
  });

  it('shows a cleared clip as empty instead of resurrecting the old prefix', () => {
    // The user emptied the clip and saved; `withSavedText` stored content ''.
    // Falling through to the row would redisplay the pre-save prefix, and the
    // character count under it, as if the save had never applied.
    expect(previewBodyText({ ...details, content: '' }, '', 'old prefix')).toBe('');
  });
});

describe('isEditReady', () => {
  it('stays disabled until the full text is in hand', () => {
    expect(isEditReady(null, undefined)).toBe(false);
  });

  it('enables Edit from a reveal even when details were skipped', () => {
    expect(isEditReady(null, 'swordfish')).toBe(true);
  });

  it('enables Edit after details load', () => {
    expect(isEditReady(details, undefined)).toBe(true);
  });

  it('keeps Edit disabled when details fail without a reveal', () => {
    expect(isEditReady(null, undefined)).toBe(false);
  });
});

describe('withSavedText', () => {
  it('replaces details.content so a later Edit does not reopen the old string', () => {
    expect(withSavedText(details, 'new body', 'keep me').content).toBe('new body');
    expect(withSavedText(details, 'new body', 'keep me').notes).toBe('keep me');
  });

  it('synthesizes details after saving a revealed clip that never fetched them', () => {
    expect(withSavedText(null, 'new body', 'a note')).toEqual({
      content: 'new body',
      ocr_text: null,
      image_expired: false,
      ocr_layout: null,
      notes: 'a note',
    });
  });
});

describe('withSavedOcrText', () => {
  it('replaces ocr_text so Scan text does not reopen the pre-correction reading', () => {
    expect(withSavedOcrText(details, 'fixed scan')?.ocr_text).toBe('fixed scan');
  });

  it('trims whitespace so Scan does not reopen an unsaved padded reading', () => {
    expect(withSavedOcrText(details, '  fixed scan  ')?.ocr_text).toBe('fixed scan');
  });

  it('clears recognized text when the correction is empty', () => {
    expect(withSavedOcrText(details, '   ')?.ocr_text).toBeNull();
  });

  it('rewrites drag-select word boxes to the correction (SBS-1010)', () => {
    const withLayout: ClipDetails = {
      ...details,
      ocr_text: 'htlps://exarnple.com',
      ocr_layout: {
        aspect: 2,
        words: [{ text: 'htlps://exarnple.com', x: 0.1, y: 0.2, width: 0.5, height: 0.1, line: 0 }],
      },
    };
    const saved = withSavedOcrText(withLayout, 'https://example.com');
    expect(saved?.ocr_text).toBe('https://example.com');
    expect(saved?.ocr_layout?.words).toEqual([
      { text: 'https://example.com', x: 0.1, y: 0.2, width: 0.5, height: 0.1, line: 0 },
    ]);
  });

  it('clears word boxes when the correction is emptied', () => {
    const withLayout: ClipDetails = {
      ...details,
      ocr_layout: {
        aspect: 2,
        words: [{ text: 'stale', x: 0, y: 0, width: 0.2, height: 0.1, line: 0 }],
      },
    };
    const saved = withSavedOcrText(withLayout, '   ');
    expect(saved?.ocr_text).toBeNull();
    expect(saved?.ocr_layout).toBeNull();
  });

  it('leaves a missing layout missing rather than inventing boxes', () => {
    expect(withSavedOcrText(details, 'fixed scan')?.ocr_layout).toBeNull();
  });
});
