import { describe, expect, it } from 'vitest';
import {
  ClipDetails,
  fullTextForEdit,
  isEditReady,
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

describe('isEditReady', () => {
  it('stays disabled until the full text is in hand', () => {
    expect(isEditReady(null, null, undefined)).toBe(false);
  });

  it('enables Edit from a reveal even when details were skipped', () => {
    expect(isEditReady(null, null, 'swordfish')).toBe(true);
  });

  it('enables Edit after details load or fail', () => {
    expect(isEditReady(details, null, undefined)).toBe(true);
    expect(isEditReady(null, 'failed', undefined)).toBe(true);
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

  it('clears recognized text when the correction is empty', () => {
    expect(withSavedOcrText(details, '   ')?.ocr_text).toBeNull();
  });
});
