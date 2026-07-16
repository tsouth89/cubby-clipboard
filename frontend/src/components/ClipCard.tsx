import { ClipboardItem } from '../types';
import { clsx } from 'clsx';
import { memo, useMemo } from 'react';
import { convertFileSrc } from '@tauri-apps/api/core';
import { Copy, File, Image as ImageIcon, MoreHorizontal } from 'lucide-react';
import { formatDistanceToNowStrict } from 'date-fns';
import { PREVIEW_CHAR_LIMIT } from '../constants';

interface ClipCardProps {
  clip: ClipboardItem;
  isSelected: boolean;
  onSelect: () => void;
  onPaste: () => void;
  onCopy: () => void;
  onContextMenu?: (e: React.MouseEvent) => void;
}

function sourceLabel(value: string | null, type: string) {
  if (!value) return type === 'image' ? 'Image' : 'Clipboard';
  return value.replace(/\.exe$/i, '');
}

export const ClipCard = memo(function ClipCard({
  clip,
  isSelected,
  onSelect,
  onPaste,
  onCopy,
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
    if (value.startsWith('/') || /^[A-Za-z]:[\\/]/.test(value)) {
      return convertFileSrc(value);
    }
    return `data:image/png;base64,${value}`;
  }, [clip.clip_type, clip.content]);

  const age = useMemo(() => {
    const parsed = new Date(clip.created_at);
    if (Number.isNaN(parsed.getTime())) return '';
    return formatDistanceToNowStrict(parsed, { addSuffix: true });
  }, [clip.created_at]);

  const label = sourceLabel(clip.source_app, clip.clip_type);
  const preview = clip.preview || clip.content;

  return (
    <article
      data-el="clip-card"
      data-clip-id={clip.id}
      onMouseEnter={onSelect}
      onClick={onSelect}
      onDoubleClick={onPaste}
      onContextMenu={(event) => {
        event.preventDefault();
        onContextMenu?.(event);
      }}
      className={clsx(
        'group relative flex min-h-[74px] cursor-default select-none items-center gap-3 overflow-hidden rounded-[11px] border px-3 py-2.5 transition-colors duration-100',
        isSelected
          ? 'bg-primary/12 border-primary/55 shadow-[inset_0_0_0_1px_hsl(var(--primary)/0.12)]'
          : 'border-transparent bg-white/[0.035] hover:border-white/[0.08] hover:bg-white/[0.06]'
      )}
    >
      {isSelected && <div className="absolute inset-y-2 left-0 w-[3px] rounded-r bg-primary" />}

      <div className="flex h-9 w-9 shrink-0 items-center justify-center overflow-hidden rounded-[9px] border border-white/[0.08] bg-black/20">
        {clip.source_icon ? (
          <img
            src={`data:image/png;base64,${clip.source_icon}`}
            alt=""
            className="h-5 w-5 object-contain"
          />
        ) : clip.clip_type === 'image' ? (
          <ImageIcon size={18} className="text-muted-foreground" />
        ) : (
          <File size={17} className="text-muted-foreground" />
        )}
      </div>

      <div className="min-w-0 flex-1">
        {clip.clip_type === 'image' ? (
          <div className="flex min-w-0 items-center gap-3">
            <div className="h-11 w-20 shrink-0 overflow-hidden rounded-md border border-white/10 bg-black/20">
              {imageSrc ? (
                <img src={imageSrc} alt="" className="h-full w-full object-cover" />
              ) : (
                <div className="flex h-full items-center justify-center">
                  <ImageIcon size={18} className="text-muted-foreground" />
                </div>
              )}
            </div>
            <div className="min-w-0">
              <p className="truncate text-[13px] font-medium text-foreground">Clipboard image</p>
              <p className="mt-1 truncate text-[11px] text-muted-foreground">
                {label}
                {age && <span className="px-1.5 text-muted-foreground/50">•</span>}
                {age}
              </p>
            </div>
          </div>
        ) : (
          <>
            <p className="line-clamp-2 whitespace-pre-wrap break-words text-[13px] leading-[18px] text-foreground/95">
              {preview.slice(0, PREVIEW_CHAR_LIMIT)}
            </p>
            <p className="mt-1 truncate text-[11px] text-muted-foreground">
              {label}
              {age && <span className="px-1.5 text-muted-foreground/50">•</span>}
              {age}
            </p>
          </>
        )}
      </div>

      <div
        className={clsx(
          'flex shrink-0 items-center gap-0.5 transition-opacity',
          isSelected ? 'opacity-100' : 'opacity-0 group-hover:opacity-100'
        )}
      >
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
          <Copy size={14} />
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
          <MoreHorizontal size={15} />
        </button>
      </div>
    </article>
  );
});
