import { useEffect, useRef } from 'react';
import { useTranslation } from 'react-i18next';
import { ClipboardItem } from '../types';
import { ClipCard } from './ClipCard';
import { AlertCircle, RefreshCw } from 'lucide-react';

interface ClipListProps {
  clips: ClipboardItem[];
  isLoading: boolean;
  hasMore: boolean;
  resetToken: number;
  density: 'compact' | 'comfortable';
  selectedClipId: string | null;
  loadError: boolean;
  emptyTitle: string;
  emptyDescription: string;
  onSelectClip: (clipId: string) => void;
  onPaste: (clipId: string) => void;
  onCopy: (clipId: string) => void;
  onTogglePin: (clipId: string) => void;
  onLoadMore: () => void;
  onRetry: () => void;
  onCardContextMenu?: (e: React.MouseEvent, clipId: string) => void;
}

export function ClipList({
  clips,
  isLoading,
  hasMore,
  resetToken,
  density,
  selectedClipId,
  loadError,
  emptyTitle,
  emptyDescription,
  onSelectClip,
  onPaste,
  onCopy,
  onTogglePin,
  onLoadMore,
  onRetry,
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
    if (loadError) {
      return (
        <div className="flex h-full flex-col items-center justify-center px-10 text-center">
          <AlertCircle size={22} className="mb-3 text-destructive" />
          <p className="text-sm font-medium text-foreground/90">{t('clipList.loadFailed')}</p>
          <p className="mt-1 text-xs leading-5 text-muted-foreground">
            {t('clipList.loadFailedDesc')}
          </p>
          <button
            type="button"
            onClick={onRetry}
            className="mt-4 flex items-center gap-1.5 rounded-md border border-white/[0.1] bg-white/[0.05] px-3 py-1.5 text-xs font-medium text-foreground transition-colors hover:bg-white/[0.09]"
          >
            <RefreshCw size={13} />
            {t('clipList.retry')}
          </button>
        </div>
      );
    }
    return (
      <div className="flex h-full flex-col items-center justify-center px-10 text-center">
        <p className="text-sm font-medium text-foreground/80">{emptyTitle}</p>
        <p className="mt-1 text-xs leading-5 text-muted-foreground">{emptyDescription}</p>
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
            density={density}
            isSelected={selectedClipId === clip.id}
            onSelect={() => onSelectClip(clip.id)}
            onPaste={() => onPaste(clip.id)}
            onCopy={() => onCopy(clip.id)}
            onTogglePin={() => onTogglePin(clip.id)}
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
