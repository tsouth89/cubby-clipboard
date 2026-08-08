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
  /** Whether moving the pointer over a card selects it. True in the flyout,
   *  where selection is only ever "what Enter will paste". The History window
   *  turns it off: selection there drives a preview pane, and sweeping the
   *  pointer across the list on the way to a button must not reload it. */
  selectOnHover?: boolean;
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
  selectOnHover = true,
}: ClipListProps) {
  const { t } = useTranslation();
  const listRef = useRef<HTMLDivElement>(null);
  // Arrow-key navigation can scroll a card under a stationary cursor and fire
  // mouseenter. A mousemove, including the first one after opening the flyout,
  // is direct evidence of pointer intent and may safely change selection.
  const handleCardHover = (clipId: string) =>
    selectOnHover ? () => onSelectClip(clipId) : undefined;

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
      id="clip-listbox"
      role="listbox"
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
      {/* presentation so the options stay direct children of the listbox in the
          accessibility tree; an intervening generic would make them invalid. */}
      <div role="presentation" className="space-y-1.5">
        {clips.map((clip, index) => (
          <ClipCard
            key={clip.id}
            clip={clip}
            density={density}
            posInSet={index + 1}
            // -1 is the ARIA value for "total unknown", which is the honest
            // answer while more pages remain unloaded.
            setSize={hasMore ? -1 : clips.length}
            isSelected={selectedClipId === clip.id}
            onHover={handleCardHover(clip.id)}
            onPaste={() => onPaste(clip.id)}
            onCopy={() => onCopy(clip.id)}
            onTogglePin={() => onTogglePin(clip.id)}
            onContextMenu={(event) => onCardContextMenu?.(event, clip.id)}
          />
        ))}
      </div>
      {isLoading && (
        <div role="presentation" className="flex justify-center py-3">
          <div className="h-4 w-4 animate-spin rounded-full border-2 border-primary/25 border-t-primary" />
        </div>
      )}
    </div>
  );
}
