/**
 * Shared presentation helpers for a clip. The flyout card and the History
 * window's list + preview pane describe the same clips, so the logic for
 * turning a stored payload into a source, a label, and a content kind lives
 * here instead of being duplicated per surface.
 */

export interface ImageMetadata {
  width?: number;
  height?: number;
  size_bytes?: number;
  formats?: string[];
}

/**
 * Resolve a clip's image content to something an `<img src>` accepts. Image
 * clips are stored as raw base64 PNG, but demo/asset-capture data and any
 * future asset-protocol path already carry their own scheme.
 */
export function imageSrcFromContent(content: string | null | undefined): string | null {
  if (!content) return null;
  if (
    content.startsWith('data:') ||
    content.startsWith('http://') ||
    content.startsWith('https://') ||
    content.startsWith('asset:') ||
    content.startsWith('tauri://')
  ) {
    return content;
  }
  return `data:image/png;base64,${content}`;
}

export function parseImageMetadata(metadata: string | null | undefined): ImageMetadata {
  if (!metadata) return {};
  try {
    return JSON.parse(metadata) as ImageMetadata;
  } catch {
    return {};
  }
}

export function formatBytes(bytes?: number): string | null {
  if (!bytes || bytes <= 0) return null;
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

export function sourceLabel(value: string | null, type: string): string {
  if (!value) return type === 'image' ? 'Image' : 'Clipboard';
  return value.replace(/\.exe$/i, '');
}

export function imageLabel(source: string): string {
  return /snip|screen|capture/i.test(source) ? 'Screenshot' : 'Clipboard image';
}

export function contentKind(content: string, clipType: string): string {
  const trimmed = content.trim();
  if (clipType === 'url' || /^https?:\/\/\S+$/i.test(trimmed)) return 'URL';
  if (/^[A-Za-z]:[\\/]|^\\\\[^\\]+\\/.test(trimmed)) return 'Path';
  if (
    /(^|\n)\s*(?:const|let|var|function|class|interface|type|pub fn|fn|use|import|SELECT|UPDATE|INSERT|git |cargo |pnpm |npm |sudo |curl |cd )\b/m.test(
      trimmed
    )
  ) {
    return 'Code';
  }
  if (trimmed.includes('\n')) return 'Text';
  return trimmed.length < 24 ? 'Snippet' : 'Text';
}

/** Collapse the blank-line padding that survives most copies, for previews. */
export function normalizePreviewText(content: string): string {
  return content
    .replace(/\r\n/g, '\n')
    .replace(/\n[ \t]*\n+/g, '\n')
    .trim();
}
