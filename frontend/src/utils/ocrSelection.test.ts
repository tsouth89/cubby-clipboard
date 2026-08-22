import { describe, expect, it } from 'vitest';
import {
  anchorWord,
  applyOcrTextToWords,
  isSelected,
  joinWords,
  nearestWord,
  OcrWord,
  selectedText,
  selectionBands,
  selectionRange,
  wordAt,
  wordContains,
} from './ocrSelection';

/**
 * Two lines of two words, as fractions of the image:
 *   line 0:  "hello" [0.0-0.2]   "there" [0.25-0.45]   y 0.00-0.10
 *   line 1:  "second"[0.0-0.2]   "line"  [0.25-0.45]   y 0.50-0.60
 */
const WORDS: OcrWord[] = [
  { text: 'hello', x: 0.0, y: 0.0, width: 0.2, height: 0.1, line: 0 },
  { text: 'there', x: 0.25, y: 0.0, width: 0.2, height: 0.1, line: 0 },
  { text: 'second', x: 0.0, y: 0.5, width: 0.2, height: 0.1, line: 1 },
  { text: 'line', x: 0.25, y: 0.5, width: 0.2, height: 0.1, line: 1 },
];

describe('selectionRange', () => {
  it('orders the ends so a backwards drag still selects forwards', () => {
    expect(selectionRange(3, 1)).toEqual({ start: 1, end: 3 });
    expect(selectionRange(1, 3)).toEqual({ start: 1, end: 3 });
  });

  it('is null until both ends exist', () => {
    expect(selectionRange(null, 2)).toBeNull();
    expect(selectionRange(2, null)).toBeNull();
  });
});

describe('wordContains / wordAt', () => {
  it('hits the word under the point', () => {
    expect(wordAt(WORDS, { x: 0.1, y: 0.05 })).toBe(0);
    expect(wordAt(WORDS, { x: 0.3, y: 0.55 })).toBe(3);
  });

  it('misses when the point is outside every box', () => {
    expect(wordAt(WORDS, { x: 0.8, y: 0.8 })).toBeNull();
    expect(wordContains(WORDS[0], { x: 0.21, y: 0.05 })).toBe(false);
  });
});

describe('nearestWord', () => {
  it('settles on the closest line band before choosing a word in it', () => {
    // Far to the right of line 1. A plain point-to-box distance would be
    // tempted by line 0's "there"; band-first keeps the sweep on line 1.
    expect(nearestWord(WORDS, { x: 0.95, y: 0.55 })).toBe(3);
  });

  it('picks the closest word within the chosen band', () => {
    expect(nearestWord(WORDS, { x: 0.0, y: 0.55 })).toBe(2);
    expect(nearestWord(WORDS, { x: 0.95, y: 0.05 })).toBe(1);
  });

  it('has nothing to offer for an empty layout', () => {
    expect(nearestWord([], { x: 0.5, y: 0.5 })).toBeNull();
  });
});

describe('anchorWord', () => {
  it('uses the word under the cursor when there is one', () => {
    expect(anchorWord(WORDS, { x: 0.1, y: 0.05 })).toBe(0);
  });

  it('reaches into the margin beside a line so a sweep can start there', () => {
    // Just left of "hello", within 1.5 line heights.
    expect(anchorWord(WORDS, { x: -0.05, y: 0.05 })).toBe(0);
  });

  it('refuses a point that is nowhere near any text', () => {
    expect(anchorWord(WORDS, { x: 0.9, y: 0.95 })).toBeNull();
  });
});

describe('joinWords / selectedText', () => {
  it('joins within a line with spaces and across lines with newlines', () => {
    expect(joinWords(WORDS)).toBe('hello there\nsecond line');
  });

  it('copies only the selected range, not the whole block', () => {
    expect(selectedText(WORDS, { start: 1, end: 2 })).toBe('there\nsecond');
    expect(selectedText(WORDS, { start: 0, end: 0 })).toBe('hello');
  });

  it('is empty with no selection', () => {
    expect(selectedText(WORDS, null)).toBe('');
  });

  it('clamps a range that runs past the end', () => {
    expect(selectedText(WORDS, { start: 2, end: 99 })).toBe('second line');
  });
});

describe('selectionBands', () => {
  // Band edges come out of float subtraction, so compare at display precision
  // rather than bit-for-bit.
  const rounded = (range: Parameters<typeof selectionBands>[1]) =>
    selectionBands(WORDS, range).map((band) => ({
      x: Number(band.x.toFixed(5)),
      y: Number(band.y.toFixed(5)),
      width: Number(band.width.toFixed(5)),
      height: Number(band.height.toFixed(5)),
    }));

  it('merges each line into one continuous band, closing the gaps between words', () => {
    // Whole line 0: one band spanning "hello" through "there", including the
    // 0.05 gap between them. Per-word rectangles would leave that gap unfilled.
    expect(rounded({ start: 0, end: 1 })).toEqual([{ x: 0, y: 0, width: 0.45, height: 0.1 }]);
  });

  it('emits one band per line for a selection that spans lines', () => {
    expect(rounded({ start: 1, end: 2 })).toEqual([
      { x: 0.25, y: 0, width: 0.2, height: 0.1 },
      { x: 0, y: 0.5, width: 0.2, height: 0.1 },
    ]);
  });

  it('has no bands without a selection', () => {
    expect(selectionBands(WORDS, null)).toEqual([]);
  });

  it('clamps a range that runs past the end', () => {
    expect(rounded({ start: 3, end: 99 })).toEqual([{ x: 0.25, y: 0.5, width: 0.2, height: 0.1 }]);
  });
});

describe('isSelected', () => {
  it('covers the inclusive range only', () => {
    const range = { start: 1, end: 2 };
    expect([0, 1, 2, 3].map((i) => isSelected(i, range))).toEqual([false, true, true, false]);
    expect(isSelected(1, null)).toBe(false);
  });
});

describe('applyOcrTextToWords', () => {
  it('rewrites box text for a same-length correction and keeps geometry (SBS-1010)', () => {
    // OCR misread a URL; the user saved the assembled block. Drag-select must
    // copy the correction, not the engine's spelling, from the same box.
    const words: OcrWord[] = [
      { text: 'htlps://exarnple.com', x: 0.1, y: 0.2, width: 0.5, height: 0.1, line: 0 },
    ];
    const corrected = applyOcrTextToWords(words, 'https://example.com');
    expect(corrected).toEqual([
      { text: 'https://example.com', x: 0.1, y: 0.2, width: 0.5, height: 0.1, line: 0 },
    ]);
    expect(selectedText(corrected, { start: 0, end: 0 })).toBe('https://example.com');
  });

  it('drops leftover boxes so a shorter correction cannot copy a stale word', () => {
    const corrected = applyOcrTextToWords(WORDS, 'hello there');
    expect(corrected.map((word) => word.text)).toEqual(['hello', 'there']);
    expect(selectedText(corrected, { start: 0, end: corrected.length - 1 })).not.toContain(
      'second'
    );
  });

  it('puts extra tokens on the last box instead of inventing geometry', () => {
    const corrected = applyOcrTextToWords(WORDS.slice(0, 2), 'hello there extra words');
    expect(corrected.map((word) => word.text)).toEqual(['hello', 'there extra words']);
  });

  it('clears every box when the correction is empty', () => {
    expect(applyOcrTextToWords(WORDS, '   ')).toEqual([]);
  });

  it('does not invent boxes when the layout had none', () => {
    expect(applyOcrTextToWords([], 'https://example.com')).toEqual([]);
  });
});
