import { useEffect, useRef } from 'react';
import { ClipboardItem } from '../types';
import { ClipCard } from './ClipCard';
import { AlertCircle, RefreshCw } from 'lucide-react';
import { clipListErrorSurface } from '../utils/clipLoadFailure';

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
  /** Hidden clips revealed for this session, by id. */
  revealedClips?: ReadonlyMap<string, ClipboardItem>;
  /** Reveal or re-hide a hidden clip for this session. */
  onToggleReveal?: (clipId: string) => void;
  /** Whether moving the pointer over a card selects it. True in the flyout,
   *  where selection is only ever "what Enter will paste". The History window
   *  turns it off: selection there drives a preview pane, and sweeping the
   *  pointer across the list on the way to a button must not reload it. */
  selectOnHover?: boolean;
  /** Multi-select is available (History window only). */
  selectable?: boolean;
  /** Ids currently in the multi-select set. */
  checkedIds?: ReadonlySet<string>;
  /** Toggle multi-selection for a row, given its index for shift-extend. */
  onToggleSelect?: (clipId: string, index: number, event: React.MouseEvent) => void;
  /** Makes the listbox the History window's keyboard focus owner. The flyout
   *  keeps focus in its search field and therefore leaves this off. */
  keyboardNavigation?: boolean;
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
  revealedClips,
  onToggleReveal,
  selectOnHover = true,
  selectable = false,
  checkedIds,
  onToggleSelect,
  keyboardNavigation = false,
}: ClipListProps) {
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

  const handleListKeyDown = (event: React.KeyboardEvent<HTMLDivElement>) => {
    // Interactive descendants keep their native key handling. Only the
    // listbox itself owns History's Up/Down selection model.
    if (!keyboardNavigation || event.target !== event.currentTarget) return;
    if (event.key !== 'ArrowUp' && event.key !== 'ArrowDown') return;

    event.preventDefault();
    const selectedIndex = clips.findIndex((clip) => clip.id === selectedClipId);
    const nextIndex =
      selectedIndex < 0
        ? 0
        : Math.min(
            Math.max(selectedIndex + (event.key === 'ArrowDown' ? 1 : -1), 0),
            clips.length - 1
          );
    const nextClip = clips[nextIndex];
    if (nextClip) onSelectClip(nextClip.id);
  };

  if (isLoading && clips.length === 0) {
    return (
      <div className="flex h-full items-center justify-center">
        <div className="h-6 w-6 animate-spin rounded-full border-2 border-primary/25 border-t-primary" />
      </div>
    );
  }

  const errorSurface = clipListErrorSurface(loadError, clips.length);

  if (errorSurface === 'panel') {
    return (
      <div className="flex h-full flex-col items-center justify-center px-10 text-center">
        <AlertCircle size={22} className="mb-3 text-destructive" />
        <p className="text-sm font-medium text-foreground/90">
          {'Couldn’t load clipboard history'}
        </p>
        <p className="mt-1 text-xs leading-5 text-muted-foreground">
          {'Cubby kept your data. Try loading it again.'}
        </p>
        <button
          type="button"
          onClick={onRetry}
          className="mt-4 flex items-center gap-1.5 rounded-md border border-white/[0.1] bg-white/[0.05] px-3 py-1.5 text-xs font-medium text-foreground transition-colors hover:bg-white/[0.09]"
        >
          <RefreshCw size={13} />
          {'Try again'}
        </button>
      </div>
    );
  }

  if (clips.length === 0) {
    return (
      <div className="flex h-full flex-col items-center justify-center px-10 text-center">
        <p className="text-sm font-medium text-foreground/80">{emptyTitle}</p>
        <p className="mt-1 text-xs leading-5 text-muted-foreground">{emptyDescription}</p>
      </div>
    );
  }

  const list = (
    <div
      ref={listRef}
      data-el="clip-list"
      id="clip-listbox"
      role="listbox"
      aria-label="Clipboard history"
      // History keeps DOM focus here while aria-activedescendant moves through
      // the options. That lets Narrator announce the selected clip and its
      // aria-posinset position without moving focus away from the list.
      tabIndex={keyboardNavigation ? 0 : undefined}
      aria-activedescendant={
        keyboardNavigation && selectedClipId && clips.some((clip) => clip.id === selectedClipId)
          ? `clip-option-${selectedClipId}`
          : undefined
      }
      onKeyDown={handleListKeyDown}
      onMouseDown={(event) => {
        if (!keyboardNavigation) return;
        const target = event.target as Element;
        if (target.closest('button, input, select, textarea, a, [contenteditable="true"]')) return;
        // A mouse-selected option and the next arrow press must use the same
        // composite widget. Keep nested controls focusable in their own right.
        event.currentTarget.focus({ preventScroll: true });
      }}
      className={`no-scrollbar overflow-y-auto px-2 pb-2 focus-visible:outline focus-visible:outline-2 focus-visible:outline-primary/70 ${errorSurface === 'banner' ? 'min-h-0 flex-1' : 'h-full'}`}
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
            revealed={revealedClips?.get(clip.id)}
            onToggleReveal={onToggleReveal ? () => onToggleReveal(clip.id) : undefined}
            selectable={selectable}
            isChecked={checkedIds?.has(clip.id) ?? false}
            onToggleSelect={(event) => onToggleSelect?.(clip.id, index, event)}
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

  if (errorSurface !== 'banner') {
    return list;
  }

  // Banner lives outside the listbox so the options stay its only children.
  // A failed same-filter refresh used to keep these rows with no in-list
  // error (SBS-805); the toast dismissed and the page looked current.
  return (
    <div className="flex h-full min-h-0 flex-col">
      <div
        role="alert"
        data-el="clip-list-stale"
        className="mx-2 mb-2 flex items-center gap-2 rounded-md border border-destructive/30 bg-destructive/10 px-3 py-2"
      >
        <AlertCircle size={14} className="shrink-0 text-destructive" />
        <p className="min-w-0 flex-1 text-xs font-medium text-foreground/90">
          {'Couldn’t refresh clipboard history'}
        </p>
        <button
          type="button"
          onClick={onRetry}
          className="flex shrink-0 items-center gap-1.5 rounded-md border border-white/[0.1] bg-white/[0.05] px-2 py-1 text-xs font-medium text-foreground transition-colors hover:bg-white/[0.09]"
        >
          <RefreshCw size={13} />
          {'Try again'}
        </button>
      </div>
      {list}
    </div>
  );
}
