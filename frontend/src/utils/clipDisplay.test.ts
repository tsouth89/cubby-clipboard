import { describe, expect, it } from 'vitest';
import {
  contentKind,
  formatBytes,
  imageLabel,
  imageSrcFromContent,
  normalizePreviewText,
  parseImageMetadata,
  sourceLabel,
} from './clipDisplay';

describe('imageSrcFromContent', () => {
  it('wraps raw base64 in a PNG data URL', () => {
    expect(imageSrcFromContent('iVBORw0KGgo=')).toBe('data:image/png;base64,iVBORw0KGgo=');
  });

  it('passes through content that already carries a scheme', () => {
    for (const src of [
      'data:image/jpeg;base64,abc',
      'http://example.test/a.png',
      'https://example.test/a.png',
      'asset://localhost/a.png',
      'tauri://localhost/a.png',
    ]) {
      expect(imageSrcFromContent(src)).toBe(src);
    }
  });

  it('returns null when there is nothing to render', () => {
    expect(imageSrcFromContent('')).toBeNull();
    expect(imageSrcFromContent(null)).toBeNull();
    expect(imageSrcFromContent(undefined)).toBeNull();
  });
});

describe('parseImageMetadata', () => {
  it('reads dimensions and size out of the stored JSON', () => {
    expect(parseImageMetadata('{"width":1920,"height":1080,"size_bytes":2048}')).toEqual({
      width: 1920,
      height: 1080,
      size_bytes: 2048,
    });
  });

  it('degrades to an empty record rather than throwing on bad JSON', () => {
    expect(parseImageMetadata('{not json')).toEqual({});
    expect(parseImageMetadata(null)).toEqual({});
  });
});

describe('formatBytes', () => {
  it('scales the unit to the magnitude', () => {
    expect(formatBytes(512)).toBe('512 B');
    expect(formatBytes(2048)).toBe('2 KB');
    expect(formatBytes(3 * 1024 * 1024)).toBe('3.0 MB');
  });

  it('renders zero as a real size rather than hiding the row', () => {
    expect(formatBytes(0)).toBe('0 B');
  });

  it('reports nothing only when the size is unknown', () => {
    expect(formatBytes(undefined)).toBeNull();
    expect(formatBytes(-1)).toBeNull();
  });
});

describe('sourceLabel', () => {
  it('strips the .exe suffix from a captured source app', () => {
    expect(sourceLabel('Code.exe', 'text')).toBe('Code');
  });

  it('falls back by clip type when no source was captured', () => {
    expect(sourceLabel(null, 'image')).toBe('Image');
    expect(sourceLabel(null, 'text')).toBe('Clipboard');
  });
});

describe('imageLabel', () => {
  it('calls out screenshots from known capture tools', () => {
    expect(imageLabel('Snipping Tool')).toBe('Screenshot');
    expect(imageLabel('ScreenSketch')).toBe('Screenshot');
    expect(imageLabel('Figma')).toBe('Clipboard image');
  });
});

describe('contentKind', () => {
  it('recognizes URLs by clip type or shape', () => {
    expect(contentKind('anything', 'url')).toBe('URL');
    expect(contentKind('https://example.test/a', 'text')).toBe('URL');
  });

  it('recognizes Windows and UNC paths', () => {
    expect(contentKind('C:\\Users\\me\\notes.txt', 'text')).toBe('Path');
    expect(contentKind('\\\\server\\share', 'text')).toBe('Path');
  });

  it('recognizes code by leading keyword', () => {
    expect(contentKind('const x = 1', 'text')).toBe('Code');
    expect(contentKind('SELECT * FROM clips', 'text')).toBe('Code');
  });

  it('separates short snippets from longer prose', () => {
    expect(contentKind('hello', 'text')).toBe('Snippet');
    expect(contentKind('a'.repeat(40), 'text')).toBe('Text');
    expect(contentKind('two\nlines', 'text')).toBe('Text');
  });
});

describe('normalizePreviewText', () => {
  it('collapses blank-line padding and normalizes line endings', () => {
    expect(normalizePreviewText('\r\n a \r\n\r\n b \r\n')).toBe('a \n b');
  });
});
