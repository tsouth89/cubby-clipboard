export const LAYOUT = {
  WINDOW_WIDTH: 520,
  WINDOW_HEIGHT: 620,
  MIN_WINDOW_HEIGHT: 300,
  HEADER_HEIGHT: 112,
  FOOTER_HEIGHT: 38,
  ROW_GAP: 6,
  WINDOW_PADDING: 10,
};

export const PREVIEW_CHAR_LIMIT = 420;

/**
 * Cap on a clip note. A note is a short reminder of what a clip is, so this is
 * generous for that job while stopping an accidental paste of a whole document
 * from being encrypted into the column and trigram-indexed. Mirrored in
 * `set_clip_notes_in_database`, which enforces it for non-UI callers too.
 */
export const NOTE_CHAR_LIMIT = 500;
