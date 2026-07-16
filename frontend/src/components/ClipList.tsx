import { useEffect, useRef } from 'react';
import { useTranslation } from 'react-i18next';
import { ClipboardItem } from '../types';
import { ClipCard } from './ClipCard';

interface ClipListProps {
  clips: ClipboardItem[];
  isLoading: boolean;
  hasMore: boolean;
  resetToken: number;
  selectedClipId: string | null;
  onSelectClip: (clipId: string) => void;
  onPaste: (clipId: string) => void;
  onCopy: (clipId: string) => void;
  onLoadMore: () => void;
  onCardContextMenu?: (e: React.MouseEvent, clipId: string) => void;
}

export function ClipList({
  clips,
  isLoading,
  hasMore,
  resetToken,
  selectedClipId,
  onSelectClip,
  onPaste,
  onCopy,
  onLoadMore,
  onCardContextMenu,
}: ClipListProps) {
  const { t } = useTranslation();
  const listRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    listRef.current?.scrollTo({ top: 0 });
  }, [resetToken]);

  useEffect(() => {
    if (!selectedClipId) return;
    listRef.current
      ?.querySelector<HTMLElement>(`[data-clip-id="${CSS.escape(selectedClipId)}"]`)
      ?.scrollIntoView({ block: 'nearest' });
  }, [selectedClipId]);

  if (isLoading && clips.length === 0) {
    return (
      <div className="flex h-full items-center justify-center">
        <div className="h-6 w-6 animate-spin rounded-full border-2 border-primary/25 border-t-primary" />
      </div>
    );
  }

  if (clips.length === 0) {
    return (
      <div className="flex h-full flex-col items-center justify-center px-10 text-center">
        <p className="text-sm font-medium text-foreground/80">{t('clipList.empty')}</p>
        <p className="mt-1 text-xs leading-5 text-muted-foreground">{t('clipList.emptyDesc')}</p>
      </div>
    );
  }

  return (
    <div
      ref={listRef}
      data-el="clip-list"
      role="list"
      aria-label="Clipboard history"
      className="no-scrollbar h-full overflow-y-auto px-2 pb-2"
      onScroll={(event) => {
        if (!hasMore || isLoading) return;
        const element = event.currentTarget;
        if (element.scrollHeight - element.scrollTop - element.clientHeight < 120) {
          onLoadMore();
        }
      }}
    >
      <div className="space-y-1.5">
        {clips.map((clip) => (
          <ClipCard
            key={clip.id}
            clip={clip}
            isSelected={selectedClipId === clip.id}
            onSelect={() => onSelectClip(clip.id)}
            onPaste={() => onPaste(clip.id)}
            onCopy={() => onCopy(clip.id)}
            onContextMenu={(event) => onCardContextMenu?.(event, clip.id)}
          />
        ))}
      </div>
      {isLoading && (
        <div className="flex justify-center py-3">
          <div className="h-4 w-4 animate-spin rounded-full border-2 border-primary/25 border-t-primary" />
        </div>
      )}
    </div>
  );
}
