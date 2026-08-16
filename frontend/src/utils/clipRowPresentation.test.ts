import { describe, expect, it } from 'vitest';
import {
  canCopyFullImage,
  fullImageCopyState,
  fullImageCopyTitle,
  shouldRenderClipRowNote,
} from './clipRowPresentation';

describe('shouldRenderClipRowNote', () => {
  it('shows a screenshot note the same as a text note (SBS-807)', () => {
    const note = 'meeting whiteboard';
    expect(shouldRenderClipRowNote(note, { hidden: false, clipType: 'image' })).toBe(true);
    expect(shouldRenderClipRowNote(note, { hidden: false, clipType: 'text' })).toBe(true);
  });

  it('treats a note of "0" as present, not empty', () => {
    expect(shouldRenderClipRowNote('0', { hidden: false, clipType: 'image' })).toBe(true);
  });

  it('hides an empty or missing note on every clip type', () => {
    for (const clipType of ['image', 'text']) {
      expect(shouldRenderClipRowNote(null, { hidden: false, clipType })).toBe(false);
      expect(shouldRenderClipRowNote('', { hidden: false, clipType })).toBe(false);
      expect(shouldRenderClipRowNote(undefined, { hidden: false, clipType })).toBe(false);
    }
  });

  it('keeps notes off a still-hidden row until reveal', () => {
    expect(shouldRenderClipRowNote('secret', { hidden: true, clipType: 'image' })).toBe(false);
  });
});

describe('fullImageCopyState', () => {
  it('disables Copy image once the full bitmap has expired (SBS-807)', () => {
    expect(fullImageCopyState(true)).toBe('expired');
    expect(canCopyFullImage(fullImageCopyState(true))).toBe(false);
    expect(fullImageCopyTitle('expired')).toBe('Full image expired; only the thumbnail remains');
  });

  it('enables Copy image only when the original is known to be present', () => {
    expect(fullImageCopyState(false)).toBe('ready');
    expect(canCopyFullImage(fullImageCopyState(false))).toBe(true);
  });

  it('treats a missing expiry flag as unknown, not ready', () => {
    expect(fullImageCopyState(undefined)).toBe('unknown');
    expect(fullImageCopyState(null)).toBe('unknown');
    expect(canCopyFullImage(fullImageCopyState(undefined))).toBe(false);
    expect(fullImageCopyTitle('unknown')).toBe('Copy image is unavailable until the image loads');
  });
});
