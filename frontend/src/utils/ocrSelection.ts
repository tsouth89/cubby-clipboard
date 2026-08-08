/**
 * Word-level text selection over an image's OCR layout — the "highlight text in
 * a screenshot like it's a web page" behavior.
 *
 * The model is ported from Matteshot's select-text mode, which solved the same
 * problem natively. Words arrive in reading order (lines top to bottom, words
 * left to right), so a selection is just a contiguous index range and the
 * geometry work is limited to deciding which word a pointer means.
 *
 * All coordinates here are fractions of the image (0..1), the form the backend
 * hands over, so nothing in this file needs to know the display size or zoom.
 */

export interface OcrWord {
  text: string;
  x: number;
  y: number;
  width: number;
  height: number;
  line: number;
}

export interface OcrLayout {
  /** Image width/height, for letterboxing the viewer to its true shape. */
  aspect: number;
  words: OcrWord[];
}

export interface Point {
  x: number;
  y: number;
}

/** Inclusive index range, low end first. */
export type SelectionRange = { start: number; end: number } | null;

export function selectionRange(anchor: number | null, focus: number | null): SelectionRange {
  if (anchor === null || focus === null) return null;
  return { start: Math.min(anchor, focus), end: Math.max(anchor, focus) };
}

export function wordContains(word: OcrWord, point: Point): boolean {
  return (
    point.x >= word.x &&
    point.x <= word.x + word.width &&
    point.y >= word.y &&
    point.y <= word.y + word.height
  );
}

/** How far a point sits above or below a word's band. Zero when inside it. */
function lineDistance(word: OcrWord, y: number): number {
  if (y < word.y) return word.y - y;
  if (y > word.y + word.height) return y - (word.y + word.height);
  return 0;
}

/** How far a point sits left or right of a word. Zero when inside it. */
function columnDistance(word: OcrWord, x: number): number {
  if (x < word.x) return word.x - x;
  if (x > word.x + word.width) return x - (word.x + word.width);
  return 0;
}

/**
 * The word to extend a selection to: settle on the closest line band first,
 * then the closest word within it. Plain point-to-box distance jumps between
 * lines mid-sweep and feels broken.
 */
export function nearestWord(words: OcrWord[], point: Point): number | null {
  if (words.length === 0) return null;

  let bandWord = words[0];
  for (const word of words) {
    if (lineDistance(word, point.y) < lineDistance(bandWord, point.y)) bandWord = word;
  }

  let best: number | null = null;
  for (let index = 0; index < words.length; index += 1) {
    if (words[index].line !== bandWord.line) continue;
    if (
      best === null ||
      columnDistance(words[index], point.x) < columnDistance(words[best], point.x)
    ) {
      best = index;
    }
  }
  return best;
}

/** The first word containing the point, if any. */
export function wordAt(words: OcrWord[], point: Point): number | null {
  const index = words.findIndex((word) => wordContains(word, point));
  return index === -1 ? null : index;
}

/**
 * The word to anchor a selection on: the one under the cursor, or the nearest
 * one within reach, so a sweep can start in the margin beside a paragraph
 * rather than having to land exactly on the first character.
 */
export function anchorWord(words: OcrWord[], point: Point): number | null {
  const hit = wordAt(words, point);
  if (hit !== null) return hit;

  const index = nearestWord(words, point);
  if (index === null) return null;
  const word = words[index];
  const reach = Math.max(word.height, 0.005) * 1.5;
  return lineDistance(word, point.y) <= reach && columnDistance(word, point.x) <= reach
    ? index
    : null;
}

/**
 * Words as text: spaces within a line, newlines between them. This is why the
 * line index is carried all the way from the OCR engine — without it a
 * multi-line selection copies back as one run-on.
 */
export function joinWords(words: OcrWord[]): string {
  let text = '';
  let line: number | null = null;
  for (const word of words) {
    if (line !== null) text += line === word.line ? ' ' : '\n';
    text += word.text;
    line = word.line;
  }
  return text;
}

/** The text of a selection range over the layout. Empty when nothing is selected. */
export function selectedText(words: OcrWord[], range: SelectionRange): string {
  if (!range) return '';
  return joinWords(words.slice(range.start, Math.min(range.end, words.length - 1) + 1));
}

/** Whether an index falls inside the selection. */
export function isSelected(index: number, range: SelectionRange): boolean {
  return range !== null && index >= range.start && index <= range.end;
}

/** A drawn highlight rectangle, as fractions of the image. */
export interface SelectionBand {
  x: number;
  y: number;
  width: number;
  height: number;
}

/**
 * The selection as one continuous band per line rather than a rectangle per
 * word. This is what makes it read as highlighted text: native selection draws
 * a single run per line, so the gaps between words stay filled and there is no
 * grid of outlined boxes.
 */
export function selectionBands(words: OcrWord[], range: SelectionRange): SelectionBand[] {
  if (!range) return [];

  const bands = new Map<number, { left: number; top: number; right: number; bottom: number }>();
  for (let index = range.start; index <= Math.min(range.end, words.length - 1); index += 1) {
    const word = words[index];
    const current = bands.get(word.line);
    if (!current) {
      bands.set(word.line, {
        left: word.x,
        top: word.y,
        right: word.x + word.width,
        bottom: word.y + word.height,
      });
      continue;
    }
    current.left = Math.min(current.left, word.x);
    current.top = Math.min(current.top, word.y);
    current.right = Math.max(current.right, word.x + word.width);
    current.bottom = Math.max(current.bottom, word.y + word.height);
  }

  return [...bands.values()].map((band) => ({
    x: band.left,
    y: band.top,
    width: band.right - band.left,
    height: band.bottom - band.top,
  }));
}
