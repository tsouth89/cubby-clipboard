import { ClipboardItem } from '../types';
import { clsx } from 'clsx';
import { memo, useMemo } from 'react';
import { Clock, Copy, File, Image as ImageIcon, MoreHorizontal, Pin } from 'lucide-react';
import { formatDistanceToNowStrict } from 'date-fns';
import { PREVIEW_CHAR_LIMIT } from '../constants';
import { useTimeTick } from '../hooks/useTimeTick';
import {
  contentKind,
  formatBytes,
  imageLabel,
  imageSrcFromContent,
  normalizePreviewText,
  parseImageMetadata,
  sourceLabel,
} from '../utils/clipDisplay';

interface ClipCardProps {
  clip: ClipboardItem;
  density: 'compact' | 'comfortable';
  isSelected: boolean;
  /** Fired only on real mouse movement. A card sliding under a stationary
   *  cursor during keyboard navigation must not steal selection. Omitted where
   *  hovering should not change the selection at all. */
  onHover?: () => void;
  onPaste: () => void;
  onCopy: () => void;
  onTogglePin: () => void;
  onContextMenu?: (e: React.MouseEvent) => void;
  /** 1-based position of this option within the full history. */
  posInSet: number;
  /** Total options in the history, or -1 when more pages remain unloaded. */
  setSize: number;
  /** Multi-select is available (History window only; the flyout omits this). */
  selectable?: boolean;
  /** Whether this row is in the multi-select set. */
  isChecked?: boolean;
  /** Toggle multi-selection. The event carries the modifiers, so the caller can
   *  tell a plain toggle from a shift-extend. */
  onToggleSelect?: (event: React.MouseEvent) => void;
}

export const ClipCard = memo(function ClipCard({
  clip,
  density,
  isSelected,
  onHover,
  onPaste,
  onCopy,
  onTogglePin,
  onContextMenu,
  posInSet,
  setSize,
  selectable = false,
  isChecked = false,
  onToggleSelect,
}: ClipCardProps) {
  const imageSrc = useMemo(
    () => (clip.clip_type === 'image' ? imageSrcFromContent(clip.content) : null),
    [clip.clip_type, clip.content]
  );

  // Ticks every 15s so the relative time stays current while the flyout is open.
  const timeTick = useTimeTick();
  const age = useMemo(() => {
    const parsed = new Date(clip.created_at);
    if (Number.isNaN(parsed.getTime())) return '';
    return formatDistanceToNowStrict(parsed, { addSuffix: true });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [clip.created_at, timeTick]);

  const label = sourceLabel(clip.source_app, clip.clip_type);
  const preview = normalizePreviewText(clip.content || clip.preview);
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
  const highlights = clip.ocr_highlights ?? null;
  const showHighlights = !!(imageSrc && highlights && highlights.boxes.length > 0);
  // Aspect of the fixed thumbnail box; used to letterbox a highlighted image to
  // its true aspect so the overlay rectangles line up in both densities.
  const thumbAspect = isCompact ? 92 / 52 : 120 / 68;
  const imageIsWide = highlights ? highlights.aspect >= thumbAspect : true;

  return (
    <article
      data-el="clip-card"
      data-clip-id={clip.id}
      data-selected={isSelected}
      // The list is single-select and keyboard-navigated, so option/aria-selected
      // is what conveys the selection. aria-current on a listitem does not: a
      // screen reader announces neither which clip is selected nor its position.
      // The id is the aria-activedescendant target the search input points at.
      id={`clip-option-${clip.id}`}
      role="option"
      aria-selected={isSelected}
      // The list is paginated, so the DOM holds only part of the history. Without
      // these a screen reader counts the rendered rows and announces a position
      // out of the wrong total.
      aria-posinset={posInSet}
      aria-setsize={setSize}
      onMouseMove={onHover}
      // Ctrl/Shift+click is a multi-select gesture wherever multi-select
      // exists, matching how file lists behave; a plain click keeps its normal
      // meaning (paste in the flyout, preview in the History window).
      onClick={(event) => {
        if (selectable && onToggleSelect && (event.ctrlKey || event.metaKey || event.shiftKey)) {
          event.preventDefault();
          onToggleSelect(event);
          return;
        }
        onPaste();
      }}
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

      {selectable && (
        <input
          type="checkbox"
          checked={isChecked}
          // The row's own click handler owns the modifier gestures; the box is
          // the plain, discoverable way to toggle one row.
          onClick={(event) => {
            event.stopPropagation();
            onToggleSelect?.(event);
          }}
          onChange={() => undefined}
          className="h-4 w-4 shrink-0 accent-primary"
          aria-label={isChecked ? 'Deselect clip' : 'Select clip'}
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
                'relative flex shrink-0 items-center justify-center overflow-hidden rounded-lg border border-white/10 bg-black/20',
                isCompact ? 'h-[52px] w-[92px]' : 'h-[68px] w-[120px]'
              )}
            >
              {!imageSrc ? (
                <div className="flex h-full items-center justify-center">
                  <ImageIcon size={20} className="text-muted-foreground" />
                </div>
              ) : showHighlights && highlights ? (
                // Letterbox to the image's true aspect so the matched-word boxes
                // (stored as fractions of the image) map straight to percentages
                // here — nothing is cropped, so every match stays visible.
                <div
                  className={clsx('relative overflow-hidden', imageIsWide ? 'w-full' : 'h-full')}
                  style={{ aspectRatio: String(highlights.aspect) }}
                >
                  <img
                    src={imageSrc}
                    alt=""
                    className={clsx(
                      'h-full w-full object-cover',
                      clip.image_expired && 'opacity-60'
                    )}
                  />
                  {highlights.boxes.map((box, index) => (
                    <div
                      key={index}
                      data-el="ocr-highlight"
                      className="pointer-events-none absolute rounded-[1px] bg-primary/30 ring-1 ring-primary/70"
                      style={{
                        left: `${box.x * 100}%`,
                        top: `${box.y * 100}%`,
                        width: `${box.width * 100}%`,
                        height: `${box.height * 100}%`,
                      }}
                    />
                  ))}
                </div>
              ) : (
                <img
                  src={imageSrc}
                  alt=""
                  className={clsx('h-full w-full object-cover', clip.image_expired && 'opacity-60')}
                />
              )}
              {clip.image_expired && (
                <div
                  data-el="image-expired-badge"
                  className="absolute bottom-1 left-1 right-1 flex items-center justify-center gap-1 rounded bg-black/70 px-1 py-[3px] text-[9px] font-medium uppercase tracking-wide text-foreground/85 backdrop-blur-sm"
                  title="Full image expired by retention — recognized text kept and searchable"
                >
                  <Clock size={9} className="shrink-0" />
                  Text only
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
