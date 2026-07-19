import { ClipboardItem } from '../types';
import { clsx } from 'clsx';
import { memo, useMemo } from 'react';
import { Copy, File, Image as ImageIcon, MoreHorizontal, Pin } from 'lucide-react';
import { formatDistanceToNowStrict } from 'date-fns';
import { PREVIEW_CHAR_LIMIT } from '../constants';
import { useTimeTick } from '../hooks/useTimeTick';

interface ClipCardProps {
  clip: ClipboardItem;
  density: 'compact' | 'comfortable';
  isSelected: boolean;
  onSelect: () => void;
  onPaste: () => void;
  onCopy: () => void;
  onTogglePin: () => void;
  onContextMenu?: (e: React.MouseEvent) => void;
}

interface ImageMetadata {
  width?: number;
  height?: number;
  size_bytes?: number;
  formats?: string[];
}

function sourceLabel(value: string | null, type: string) {
  if (!value)
    return type === 'image' ? 'Image' : type === 'file' || type === 'files' ? 'Files' : 'Clipboard';
  return value.replace(/\.exe$/i, '');
}

function parseImageMetadata(metadata: string | null): ImageMetadata {
  if (!metadata) return {};
  try {
    return JSON.parse(metadata) as ImageMetadata;
  } catch {
    return {};
  }
}

function formatBytes(bytes?: number) {
  if (!bytes || bytes <= 0) return null;
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

function contentKind(content: string, clipType: string) {
  if (clipType === 'file' || clipType === 'files') return 'Files';
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

function imageLabel(source: string) {
  return /snip|screen|capture/i.test(source) ? 'Screenshot' : 'Clipboard image';
}

export const ClipCard = memo(function ClipCard({
  clip,
  density,
  isSelected,
  onSelect,
  onPaste,
  onCopy,
  onTogglePin,
  onContextMenu,
}: ClipCardProps) {
  const imageSrc = useMemo(() => {
    if (clip.clip_type !== 'image' || !clip.content) return null;
    const value = clip.content;
    if (
      value.startsWith('data:') ||
      value.startsWith('http://') ||
      value.startsWith('https://') ||
      value.startsWith('asset:') ||
      value.startsWith('tauri://')
    ) {
      return value;
    }
    return `data:image/png;base64,${value}`;
  }, [clip.clip_type, clip.content]);

  // Ticks every 15s so the relative time stays current while the flyout is open.
  const timeTick = useTimeTick();
  const age = useMemo(() => {
    const parsed = new Date(clip.created_at);
    if (Number.isNaN(parsed.getTime())) return '';
    return formatDistanceToNowStrict(parsed, { addSuffix: true });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [clip.created_at, timeTick]);

  const label = sourceLabel(clip.source_app, clip.clip_type);
  const preview = (clip.content || clip.preview)
    .replace(/\r\n/g, '\n')
    .replace(/\n[ \t]*\n+/g, '\n')
    .trim();
  const imageMetadata = useMemo(() => parseImageMetadata(clip.metadata), [clip.metadata]);
  const kind = imageMetadata.formats?.some((format) => format === 'html' || format === 'rtf')
    ? 'Rich text'
    : contentKind(preview, clip.clip_type);
  const imageDetails = [
    imageMetadata.width && imageMetadata.height
      ? `${imageMetadata.width}×${imageMetadata.height}`
      : null,
    formatBytes(imageMetadata.size_bytes),
  ].filter(Boolean);
  const isCompact = density === 'compact';

  return (
    <article
      data-el="clip-card"
      data-clip-id={clip.id}
      data-selected={isSelected}
      role="listitem"
      aria-current={isSelected ? 'true' : undefined}
      onMouseEnter={onSelect}
      onClick={onPaste}
      onContextMenu={(event) => {
        event.preventDefault();
        onContextMenu?.(event);
      }}
      className={clsx(
        'group relative flex cursor-default select-none items-center overflow-hidden rounded-[10px] border transition-colors duration-100',
        isCompact ? 'min-h-[72px] gap-2 px-2.5 py-2' : 'min-h-[92px] gap-2.5 px-3 py-2.5',
        isSelected
          ? 'border-white/[0.1] bg-white/[0.09]'
          : 'border-transparent bg-white/[0.035] hover:border-white/[0.075] hover:bg-white/[0.065]'
      )}
    >
      {isSelected && (
        <div
          className={clsx(
            'absolute left-0 w-[3px] rounded-r bg-primary',
            isCompact ? 'inset-y-2' : 'inset-y-2.5'
          )}
        />
      )}

      <div
        className={clsx(
          'flex shrink-0 items-center justify-center overflow-hidden rounded-lg border border-white/[0.075] bg-black/15',
          isCompact ? 'h-7 w-7' : 'h-8 w-8'
        )}
      >
        {clip.source_icon ? (
          <img
            src={`data:image/png;base64,${clip.source_icon}`}
            alt=""
            className="h-[18px] w-[18px] object-contain"
          />
        ) : clip.clip_type === 'image' ? (
          <ImageIcon size={16} className="text-muted-foreground" />
        ) : (
          <File size={15} className="text-muted-foreground" />
        )}
      </div>

      <div className="min-w-0 flex-1">
        {clip.clip_type === 'image' ? (
          <div className="flex min-w-0 items-center gap-3">
            <div
              className={clsx(
                'shrink-0 overflow-hidden rounded-lg border border-white/10 bg-black/20',
                isCompact ? 'h-[52px] w-[92px]' : 'h-[68px] w-[120px]'
              )}
            >
              {imageSrc ? (
                <img src={imageSrc} alt="" className="h-full w-full object-cover" />
              ) : (
                <div className="flex h-full items-center justify-center">
                  <ImageIcon size={20} className="text-muted-foreground" />
                </div>
              )}
            </div>
            <div className="min-w-0">
              <p className="truncate text-[13px] font-medium text-foreground">
                {imageLabel(label)}
              </p>
              {clip.ocr_match ? (
                <p
                  data-el="ocr-match"
                  className="mt-1 line-clamp-2 break-words text-[11px] leading-[15px] text-foreground/65"
                  title={`${clip.ocr_match.before}${clip.ocr_match.matched}${clip.ocr_match.after}`}
                >
                  {clip.ocr_match.before}
                  <mark className="rounded-[3px] bg-primary/25 px-0.5 font-medium text-foreground">
                    {clip.ocr_match.matched}
                  </mark>
                  {clip.ocr_match.after}
                </p>
              ) : imageDetails.length > 0 ? (
                <p className="mt-1 truncate text-[11px] text-foreground/55">
                  {imageDetails.join(' · ')}
                </p>
              ) : null}
              <p className="mt-1.5 truncate text-[11px] text-muted-foreground">
                {label}
                {age && <span className="px-1.5 text-muted-foreground/40">•</span>}
                {age}
              </p>
            </div>
          </div>
        ) : (
          <>
            <p
              className={clsx(
                'whitespace-pre-wrap break-words text-[13px] text-foreground/95',
                isCompact ? 'line-clamp-2 leading-[17px]' : 'line-clamp-3 leading-[18px]',
                kind === 'Code' && 'font-mono text-[12px] leading-[17px] text-foreground/90'
              )}
            >
              {preview.slice(0, PREVIEW_CHAR_LIMIT)}
            </p>
            <div className="mt-1.5 flex min-w-0 items-center gap-1.5 text-[11px] text-muted-foreground">
              {clip.is_pinned && (
                <>
                  <Pin size={10} className="shrink-0 fill-current text-primary" />
                  <span className="shrink-0 text-foreground/65">Pinned</span>
                  <span className="shrink-0 text-muted-foreground/35">•</span>
                </>
              )}
              <span className="truncate">{label}</span>
              <span className="shrink-0 text-muted-foreground/35">•</span>
              <span className="shrink-0 text-foreground/50">{kind}</span>
              {age && (
                <>
                  <span className="shrink-0 text-muted-foreground/35">•</span>
                  <span className="shrink-0">{age}</span>
                </>
              )}
            </div>
          </>
        )}
      </div>

      <div
        className={clsx(
          'absolute right-2 top-2 flex items-center gap-0.5 rounded-lg border border-white/[0.06] bg-[#202023]/95 p-0.5 shadow-lg transition-opacity',
          isSelected ? 'opacity-100' : 'opacity-0 group-hover:opacity-100'
        )}
      >
        <button
          type="button"
          onClick={(event) => {
            event.stopPropagation();
            onTogglePin();
          }}
          className={clsx(
            'rounded-md p-1.5 transition-colors hover:bg-white/10 hover:text-foreground',
            clip.is_pinned ? 'text-primary' : 'text-muted-foreground'
          )}
          title={clip.is_pinned ? 'Unpin' : 'Pin'}
          aria-label={clip.is_pinned ? 'Unpin clip' : 'Pin clip'}
          aria-pressed={clip.is_pinned}
        >
          <Pin size={13} className={clsx(clip.is_pinned && 'fill-current')} />
        </button>
        <button
          type="button"
          onClick={(event) => {
            event.stopPropagation();
            onCopy();
          }}
          className="rounded-md p-1.5 text-muted-foreground transition-colors hover:bg-white/10 hover:text-foreground"
          title="Copy"
          aria-label="Copy clip"
        >
          <Copy size={13} />
        </button>
        <button
          type="button"
          onClick={(event) => {
            event.stopPropagation();
            onContextMenu?.(event);
          }}
          className="rounded-md p-1.5 text-muted-foreground transition-colors hover:bg-white/10 hover:text-foreground"
          title="More actions"
          aria-label="More clip actions"
        >
          <MoreHorizontal size={14} />
        </button>
      </div>
    </article>
  );
});
