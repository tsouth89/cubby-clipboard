import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { CheckSquare, Copy, History, Pin, PinOff, Search, Trash2, X } from 'lucide-react';
import { Toaster, toast } from 'sonner';
import { ClipboardItem, FolderItem, Settings } from '../types';
import { ClipList } from '../components/ClipList';
import { ClipPreview } from '../components/ClipPreview';
import { ContentFilter } from '../components/FlyoutHeader';
import { useTheme } from '../hooks/useTheme';
import { useLanguage } from '../hooks/useLanguage';
import { useSystemAccent } from '../hooks/useSystemAccent';
import { useRevealedClips } from '../hooks/useRevealedClips';
import { customRange, DATE_PRESET_LABELS, DatePreset, presetRange } from '../utils/dateRange';
import {
  applySelectionClick,
  EMPTY_SELECTION,
  pruneSelection,
  selectedInOrder,
  toggleSelectAll,
} from '../utils/multiSelect';

/** One entry in the source-app filter, from `get_source_apps`. */
interface SourceAppCount {
  name: string;
  count: number;
}

const PAGE_SIZE = 20;

const bulkActionClass =
  'inline-flex items-center gap-1.5 rounded-md border border-white/[0.09] bg-white/[0.06] px-2 py-1 text-[11px] font-medium text-foreground transition-colors hover:bg-white/[0.12]';

const filterSelectClass =
  'h-9 max-w-[168px] shrink-0 rounded-md border border-white/[0.08] bg-transparent px-2 text-xs text-muted-foreground outline-none hover:text-foreground';

const CONTENT_FILTERS: ReadonlyArray<readonly [ContentFilter, string]> = [
  ['all', 'All'],
  ['text', 'Text'],
  ['images', 'Images'],
];

/**
 * The dedicated History window (SOU-582): the roomy, resizable counterpart to
 * the `Win+V` flyout. Same clips and same IPC commands; the extra space buys a
 * preview pane for the selected clip. The flyout is untouched — this window is
 * additive, and because it is not the flyout there is no focus contract to
 * honor, so its primary action is copy rather than paste.
 */
export function HistoryWindow() {
  const [clips, setClips] = useState<ClipboardItem[]>([]);
  const [folders, setFolders] = useState<FolderItem[]>([]);
  const [selectedFolder, setSelectedFolder] = useState<string | null>(null);
  const [searchQuery, setSearchQuery] = useState('');
  const [contentFilter, setContentFilter] = useState<ContentFilter>('all');
  const [selectedClipId, setSelectedClipId] = useState<string | null>(null);
  const [isLoading, setIsLoading] = useState(true);
  const [loadError, setLoadError] = useState(false);
  const [hasMore, setHasMore] = useState(true);
  const [totalClipCount, setTotalClipCount] = useState(0);
  const [settings, setSettings] = useState<Settings | null>(null);
  const [sourceApps, setSourceApps] = useState<SourceAppCount[]>([]);
  const [sourceApp, setSourceApp] = useState<string | null>(null);
  const [selection, setSelection] = useState(EMPTY_SELECTION);
  const [datePreset, setDatePreset] = useState<DatePreset>('all');
  const [customFrom, setCustomFrom] = useState('');
  const [customTo, setCustomTo] = useState('');

  // Recomputed per render rather than stored, so a window left open overnight
  // still means "today" tomorrow instead of pinning yesterday's boundaries.
  const dateRange =
    datePreset === 'custom'
      ? customRange(customFrom, customTo)
      : presetRange(datePreset, new Date());

  const effectiveTheme = useTheme(settings?.theme ?? 'system');
  useLanguage(settings?.language);
  // Each Tauri window is its own document, so this one has to apply the Windows
  // accent color itself.
  useSystemAccent();
  const density = settings?.density ?? 'comfortable';

  const { revealed, toggleReveal, forgetRevealed } = useRevealedClips();

  const clipsRef = useRef<ClipboardItem[]>(clips);
  clipsRef.current = clips;
  // Guards against an older load landing after a newer one and restoring stale
  // results — the same discipline the flyout uses.
  const loadIdRef = useRef(0);

  useEffect(() => {
    invoke<Settings>('get_settings').then(setSettings).catch(console.error);
    const unlisten = listen<Settings>('settings-changed', (event) => setSettings(event.payload));
    return () => {
      unlisten.then((dispose) => dispose()).catch(() => undefined);
    };
  }, []);

  // Destructured so the load callback depends on the two stamps rather than a
  // fresh object identity each render, which would reload on every keystroke.
  const { from: dateFrom, to: dateTo } = dateRange;

  const loadClips = useCallback(
    async (append: boolean) => {
      const loadId = ++loadIdRef.current;
      const offset = append ? clipsRef.current.length : 0;
      const query = searchQuery.trim();

      try {
        setIsLoading(true);
        setLoadError(false);

        const data = query
          ? await invoke<ClipboardItem[]>('search_clips', {
              // The trimmed form, not the raw input: the index matches on the
              // normalized text, so a stray leading space would match nothing
              // while the box still looks like it holds a real query.
              query,
              filterId: selectedFolder,
              limit: PAGE_SIZE,
              offset,
              contentFilter,
              dateFrom,
              dateTo,
              sourceApp,
            })
          : await invoke<ClipboardItem[]>('get_clips', {
              filterId: selectedFolder,
              limit: PAGE_SIZE,
              offset,
              previewOnly: true,
              contentFilter,
              dateFrom,
              dateTo,
              sourceApp,
            });

        if (loadId !== loadIdRef.current) return true;
        setClips((previous) => (append ? [...previous, ...data] : data));
        setHasMore(data.length === PAGE_SIZE);
        return true;
      } catch (error) {
        // Superseded: a newer load owns the view, so this one failing is not a
        // failure to refresh — report success and let the newer load speak.
        if (loadId !== loadIdRef.current) return true;
        console.error('Failed to load clips:', error);
        setLoadError(true);
        setHasMore(false);
        return false;
      } finally {
        if (loadId === loadIdRef.current) setIsLoading(false);
      }
    },
    [contentFilter, dateFrom, dateTo, searchQuery, selectedFolder, sourceApp]
  );

  const loadFolders = useCallback(async () => {
    try {
      setFolders(await invoke<FolderItem[]>('get_folders'));
    } catch (error) {
      console.error('Failed to load folders:', error);
    }
  }, []);

  const loadSourceApps = useCallback(async () => {
    try {
      setSourceApps(await invoke<SourceAppCount[]>('get_source_apps'));
    } catch (error) {
      console.error('Failed to load source apps:', error);
    }
  }, []);

  const refreshTotalCount = useCallback(async () => {
    try {
      setTotalClipCount(await invoke<number>('get_clipboard_history_size'));
    } catch (error) {
      console.error('Failed to get history size:', error);
    }
  }, []);

  useEffect(() => {
    loadClips(false);
  }, [loadClips]);

  // Narrowing the list replaces its contents, so send it back to the top.
  // Without this the viewport keeps the old scroll offset and a shorter result
  // set can land the user below the last row, staring at empty space.
  const [listResetToken, setListResetToken] = useState(0);
  useEffect(() => {
    setListResetToken((token) => token + 1);
  }, [contentFilter, searchQuery, selectedFolder, sourceApp, dateFrom, dateTo]);

  useEffect(() => {
    loadFolders();
    refreshTotalCount();
    loadSourceApps();
  }, [loadFolders, refreshTotalCount, loadSourceApps]);

  // Keep the window live while it sits open next to the flyout: a copy made
  // anywhere shows up here without a manual refresh.
  useEffect(() => {
    const unlistenClipboard = listen('clipboard-change', () => {
      loadClips(false);
      loadFolders();
      refreshTotalCount();
      // A new clip can introduce an app, or move one up the list.
      loadSourceApps();
    });
    const unlistenOcr = listen<string>('ocr-completed', (event) => {
      setClips((previous) =>
        previous.map((clip) => (clip.id === event.payload ? { ...clip, has_ocr_text: true } : clip))
      );
    });
    // Deleting or pinning from the flyout or tray emits no event this window
    // hears, so it would keep listing clips that are already gone and fail on
    // the next action against one. Refreshing when the window is focused again
    // catches every such change without a new cross-window event contract.
    const refreshOnFocus = () => {
      loadClips(false);
      loadFolders();
      refreshTotalCount();
      // Deleting elsewhere can empty out an app, changing its count or dropping
      // it from the filter list entirely.
      loadSourceApps();
    };
    window.addEventListener('focus', refreshOnFocus);

    return () => {
      window.removeEventListener('focus', refreshOnFocus);
      unlistenClipboard.then((dispose) => dispose()).catch(() => undefined);
      unlistenOcr.then((dispose) => dispose()).catch(() => undefined);
    };
  }, [loadClips, loadFolders, refreshTotalCount, loadSourceApps]);

  const selectedClip = useMemo(
    () => clips.find((clip) => clip.id === selectedClipId) ?? null,
    [clips, selectedClipId]
  );

  const clipOrder = useMemo(() => clips.map((clip) => clip.id), [clips]);
  // Selection is over loaded rows, so a filter change, a reload, or a delete
  // must not leave the count claiming rows that are no longer there.
  useEffect(() => {
    setSelection((current) => pruneSelection(current, clipOrder));
  }, [clipOrder]);

  const selectedIds = useMemo(() => selectedInOrder(selection, clipOrder), [selection, clipOrder]);
  const selectionCount = selectedIds.length;

  const handleToggleSelect = useCallback(
    (_clipId: string, index: number, event: React.MouseEvent) => {
      setSelection((current) =>
        applySelectionClick(current, clipOrder, index, {
          shiftKey: event.shiftKey,
          ctrlKey: event.ctrlKey,
          metaKey: event.metaKey,
        })
      );
    },
    [clipOrder]
  );

  const clearSelection = useCallback(() => setSelection(EMPTY_SELECTION), []);

  const afterBulkChange = useCallback(async () => {
    clearSelection();
    await Promise.all([loadClips(false), loadFolders(), refreshTotalCount(), loadSourceApps()]);
  }, [clearSelection, loadClips, loadFolders, refreshTotalCount, loadSourceApps]);

  const handleBulkDelete = useCallback(async () => {
    if (selectionCount === 0) return;
    try {
      // Same contract as single-clip delete: Cubby has no trash, so the payload
      // goes immediately rather than leaving a hidden soft-delete.
      const deleted = await invoke<number>('delete_clips', {
        ids: selectedIds,
        hardDelete: true,
      });
      await afterBulkChange();
      toast.success(deleted === 1 ? 'Deleted 1 clip' : `Deleted ${deleted} clips`);
    } catch (error) {
      console.error('Failed to delete clips:', error);
      toast.error('Failed to delete clips');
    }
  }, [afterBulkChange, selectedIds, selectionCount]);

  const handleBulkPin = useCallback(
    async (pinned: boolean) => {
      if (selectionCount === 0) return;
      try {
        await invoke<number>('set_clips_pinned', { ids: selectedIds, pinned });
        await afterBulkChange();
        toast.success(
          pinned ? `Pinned ${selectionCount} clips` : `Unpinned ${selectionCount} clips`
        );
      } catch (error) {
        console.error('Failed to update pin state:', error);
        toast.error('Failed to update pin state');
      }
    },
    [afterBulkChange, selectedIds, selectionCount]
  );

  const handleBulkMove = useCallback(
    async (folderId: string | null) => {
      if (selectionCount === 0) return;
      try {
        await invoke<number>('move_clips_to_folder', { ids: selectedIds, folderId });
        await afterBulkChange();
        toast.success(
          folderId
            ? `Moved ${selectionCount} clips to ${folders.find((f) => f.id === folderId)?.name ?? 'folder'}`
            : `Removed ${selectionCount} clips from their folder`
        );
      } catch (error) {
        console.error('Failed to move clips:', error);
        toast.error('Failed to move clips');
      }
    },
    [afterBulkChange, folders, selectedIds, selectionCount]
  );

  const handleBulkCopy = useCallback(async () => {
    if (selectionCount === 0) return;
    const chosen = selectedIds
      .map((id) => clipsRef.current.find((clip) => clip.id === id))
      .filter((clip): clip is ClipboardItem => Boolean(clip));
    // Images have no text to concatenate. Take their recognized text where OCR
    // found some, and say plainly how many contributed nothing.
    const parts: string[] = [];
    let skipped = 0;
    for (const clip of chosen) {
      const text = clip.clip_type === 'image' ? '' : (clip.content ?? clip.preview ?? '');
      if (text.trim()) parts.push(text);
      else skipped += 1;
    }
    if (parts.length === 0) {
      toast.error('Nothing to copy from the selected clips');
      return;
    }
    try {
      await invoke('copy_selected_text', { text: parts.join('\n\n') });
      toast.success(
        skipped > 0
          ? `Copied ${parts.length} clips (${skipped} without text skipped)`
          : `Copied ${parts.length} clips`
      );
    } catch (error) {
      console.error('Failed to copy clips:', error);
      toast.error('Failed to copy');
    }
  }, [selectedIds, selectionCount]);

  useEffect(() => {
    if (clips.length === 0) {
      setSelectedClipId(null);
      return;
    }
    if (!selectedClipId || !clips.some((clip) => clip.id === selectedClipId)) {
      setSelectedClipId(clips[0].id);
    }
  }, [clips, selectedClipId]);

  const handleCopy = useCallback(async (clipId: string, plainText = false) => {
    const clip = clipsRef.current.find((item) => item.id === clipId);
    if (!clip) return;
    // "As plain text" on an image means its recognized text, the same as
    // Shift+Enter in the flyout. Silently doing nothing looked like a success.
    if (plainText && clip.clip_type === 'image') {
      if (!clip.has_ocr_text) {
        toast.error('No recognized text on this image');
        return;
      }
      try {
        await invoke('copy_ocr_text', { id: clipId });
        toast.success('Copied');
      } catch (error) {
        console.error('Failed to copy recognized text:', error);
        toast.error('Failed to copy');
      }
      return;
    }
    if (clip.clip_type === 'image' && clip.image_expired) {
      toast.error("This screenshot's full image expired. Only its recognized text remains.");
      return;
    }
    try {
      await invoke('copy_clip', { id: clipId, plainText });
      toast.success('Copied');
    } catch (error) {
      console.error('Failed to copy clip:', error);
      toast.error('Failed to copy');
    }
  }, []);

  // Text the user picked out of an image preview, rather than the whole
  // recognized block.
  const handleCopySelection = useCallback(async (text: string) => {
    try {
      await invoke('copy_selected_text', { text });
      toast.success('Copied selection');
    } catch (error) {
      console.error('Failed to copy the selection:', error);
      toast.error('Failed to copy');
    }
  }, []);

  const handleOpenImage = useCallback(async (clipId: string) => {
    try {
      await invoke('open_image_window', { id: clipId });
    } catch (error) {
      console.error('Failed to open the image window:', error);
      toast.error('Failed to open the image');
    }
  }, []);

  const handleCopyOcrText = useCallback(async (clipId: string) => {
    try {
      await invoke('copy_ocr_text', { id: clipId });
      toast.success('Copied');
    } catch (error) {
      console.error('Failed to copy recognized text:', error);
      toast.error('Failed to copy');
    }
  }, []);

  const handleSaveText = useCallback(
    async (clipId: string, text: string) => {
      try {
        await invoke('update_clip_text', { id: clipId, text });
        // The row still holds the pre-edit preview, and the edit may have
        // changed which clips a filter or search matches.
        await loadClips(false);
        toast.success('Clip updated');
      } catch (error) {
        console.error('Failed to update the clip:', error);
        toast.error('Failed to update the clip');
      }
    },
    [loadClips]
  );
  const handleSaveNotes = useCallback(async (clipId: string, notes: string) => {
    try {
      await invoke('set_clip_notes', { id: clipId, notes });
      // Cheap local update so the row's note appears immediately; a note also
      // changes what search matches, but not the current result set.
      setClips((current) =>
        current.map((clip) => (clip.id === clipId ? { ...clip, notes: notes || null } : clip))
      );
    } catch (error) {
      console.error('Failed to save the note:', error);
      toast.error('Failed to save the note');
    }
  }, []);

  const handleSaveOcrText = useCallback(async (clipId: string, text: string) => {
    try {
      await invoke('set_clip_ocr_text', { id: clipId, text });
      // Mark the row as having recognized text so the pane refetches and the
      // "paste text" affordances appear.
      setClips((current) =>
        current.map((clip) =>
          clip.id === clipId ? { ...clip, has_ocr_text: text.trim().length > 0 } : clip
        )
      );
      toast.success('Recognized text updated');
    } catch (error) {
      console.error('Failed to save the recognized text:', error);
      toast.error('Failed to save the recognized text');
    }
  }, []);

  const handleRescanOcr = useCallback(async (clipId: string) => {
    try {
      await invoke('rescan_clip_ocr', { id: clipId });
      toast.info('Scanning this image for text…');
    } catch (error) {
      console.error('Failed to queue an OCR scan:', error);
      toast.error('Could not scan this image');
    }
  }, []);
  const handleToggleReveal = useCallback(
    (clipId: string) => {
      const clip = clipsRef.current.find((item) => item.id === clipId);
      if (clip) void toggleReveal(clip);
    },
    [toggleReveal]
  );

  /** Persist the hidden flag, as opposed to revealing for the session. */
  const handleToggleHidden = useCallback(
    async (clipId: string) => {
      try {
        const hidden = await invoke<boolean>('toggle_clip_hidden', { id: clipId });
        forgetRevealed(clipId);
        // loadClips reports failure rather than throwing, so a stale list after
        // a failed reload would otherwise be announced as success.
        if (!(await loadClips(false))) {
          toast.error('Visibility changed, but the list could not be reloaded');
          return;
        }
        toast.success(hidden ? 'Clip hidden' : 'Clip no longer hidden');
      } catch (error) {
        console.error('Failed to change clip visibility:', error);
        toast.error('Failed to change clip visibility');
      }
    },
    [forgetRevealed, loadClips]
  );

  const handleTogglePin = useCallback(async (clipId: string) => {
    try {
      const isPinned = await invoke<boolean>('toggle_clip_pin', { id: clipId });
      setClips((previous) =>
        previous
          .map((clip) => (clip.id === clipId ? { ...clip, is_pinned: isPinned } : clip))
          .sort(
            (left, right) =>
              Number(right.is_pinned) - Number(left.is_pinned) ||
              new Date(right.created_at).getTime() - new Date(left.created_at).getTime()
          )
      );
      toast.success(isPinned ? 'Clip pinned' : 'Clip unpinned');
    } catch (error) {
      console.error('Failed to update pin state:', error);
      toast.error('Failed to update pin state');
    }
  }, []);

  const handleDelete = useCallback(
    async (clipId: string) => {
      const current = clipsRef.current;
      const deletedIndex = current.findIndex((clip) => clip.id === clipId);
      const remaining = current.filter((clip) => clip.id !== clipId);
      const nextSelection =
        deletedIndex < 0
          ? (remaining[0]?.id ?? null)
          : (remaining[Math.min(deletedIndex, remaining.length - 1)]?.id ?? null);
      try {
        // Cubby has no trash, so delete removes the payload immediately rather
        // than leaving a hidden soft-delete. Same contract as the flyout.
        await invoke('delete_clip', { id: clipId, hardDelete: true });
        setClips(remaining);
        setSelectedClipId(nextSelection);
        loadFolders();
        refreshTotalCount();
        toast.success('Clip deleted');
      } catch (error) {
        console.error('Failed to delete clip:', error);
        toast.error('Failed to delete clip');
      }
    },
    [loadFolders, refreshTotalCount]
  );

  const handleClose = useCallback(() => {
    getCurrentWindow()
      .close()
      .catch((error) => console.error('Failed to close the history window:', error));
  }, []);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      const target = event.target as HTMLElement | null;
      const isEditing =
        target?.tagName === 'INPUT' || target?.tagName === 'TEXTAREA' || target?.isContentEditable;

      if (event.key === 'Escape') {
        if (searchQuery) {
          setSearchQuery('');
          return;
        }
        handleClose();
        return;
      }
      if (event.key === 'f' && (event.ctrlKey || event.metaKey)) {
        event.preventDefault();
        document.querySelector<HTMLInputElement>('[data-el="history-search-input"]')?.focus();
        return;
      }
      // Arrow keys move the list selection, but not while the caret is in the
      // search box — there they belong to the input, or typing becomes
      // unworkable without a mouse.
      if ((event.key === 'ArrowUp' || event.key === 'ArrowDown') && !isEditing) {
        const list = clipsRef.current;
        if (list.length === 0) return;
        event.preventDefault();
        const index = list.findIndex((clip) => clip.id === selectedClipId);
        const nextIndex =
          index < 0
            ? 0
            : Math.min(Math.max(index + (event.key === 'ArrowDown' ? 1 : -1), 0), list.length - 1);
        setSelectedClipId(list[nextIndex].id);
        return;
      }
      if (isEditing) return;
      if (event.key === 'Enter' && selectedClipId) {
        event.preventDefault();
        handleCopy(selectedClipId, event.shiftKey);
        return;
      }
      if (event.key === 'Delete' && selectedClipId) {
        event.preventDefault();
        handleDelete(selectedClipId);
      }
    };

    document.addEventListener('keydown', onKeyDown);
    return () => document.removeEventListener('keydown', onKeyDown);
  }, [handleClose, handleCopy, handleDelete, searchQuery, selectedClipId]);

  const hasNarrowingFilter =
    Boolean(sourceApp) || dateFrom !== null || dateTo !== null || contentFilter !== 'all';

  const emptyState = searchQuery.trim()
    ? {
        title: 'No matches',
        description: hasNarrowingFilter
          ? 'Try a different search term, or widen the filters.'
          : 'Try a different search term.',
      }
    : sourceApp
      ? {
          title: `Nothing from ${sourceApp}`,
          description: 'No clips from this app match the other filters.',
        }
      : dateFrom !== null || dateTo !== null
        ? {
            title: 'Nothing in this date range',
            description: 'Try a wider range, or Any time.',
          }
        : selectedFolder
          ? { title: 'This folder is empty', description: 'Move clips here to keep them together.' }
          : contentFilter === 'images'
            ? { title: 'No images yet', description: 'Copy an image and it will show up here.' }
            : contentFilter === 'text'
              ? {
                  title: 'No text clips yet',
                  description: 'Copy some text and it will show up here.',
                }
              : {
                  title: 'Nothing here yet',
                  description: 'Anything you copy will appear in Cubby.',
                };

  return (
    <div className="flex h-screen select-none flex-col bg-background text-foreground">
      <header
        className="flex shrink-0 items-center justify-between border-b border-border px-4 py-3"
        onMouseDown={(event) => {
          if (event.button === 0) getCurrentWindow().startDragging();
        }}
      >
        <div className="flex items-center gap-2.5">
          <History size={17} />
          <span className="text-sm font-semibold">History</span>
        </div>
        <button
          type="button"
          onClick={handleClose}
          onMouseDown={(event) => event.stopPropagation()}
          className="icon-button"
          aria-label="Close history"
        >
          <X size={18} />
        </button>
      </header>

      <div className="flex shrink-0 items-center gap-3 border-b border-border px-4 py-2.5">
        <div className="flex h-9 min-w-0 flex-1 items-center gap-2 rounded-[10px] border border-white/[0.08] bg-white/[0.055] px-3 focus-within:border-primary/45">
          <Search size={15} className="shrink-0 text-muted-foreground" />
          <input
            data-el="history-search-input"
            value={searchQuery}
            onChange={(event) => setSearchQuery(event.target.value)}
            placeholder="Search clipboard history"
            aria-label="Search clipboard history"
            className="min-w-0 flex-1 bg-transparent text-[13px] outline-none placeholder:text-muted-foreground"
          />
          {searchQuery && (
            <button
              type="button"
              onClick={() => setSearchQuery('')}
              className="rounded p-1 text-muted-foreground hover:bg-white/10 hover:text-foreground"
              aria-label="Clear search"
            >
              <X size={13} />
            </button>
          )}
        </div>

        <button
          type="button"
          onClick={() => setSelection((current) => toggleSelectAll(current, clipOrder))}
          disabled={clips.length === 0}
          className="inline-flex shrink-0 items-center gap-1.5 rounded-md border border-white/[0.08] px-2 py-1.5 text-[11px] text-muted-foreground transition-colors hover:text-foreground disabled:opacity-40"
          title="Select every clip currently loaded"
        >
          <CheckSquare size={13} />
          {selectionCount > 0 && selectionCount === clips.length ? 'None' : 'All'}
        </button>

        <div className="flex shrink-0 items-center gap-1">
          {CONTENT_FILTERS.map(([id, label]) => (
            <button
              key={id}
              type="button"
              onClick={() => setContentFilter(id)}
              aria-pressed={contentFilter === id}
              className={`rounded-md px-2.5 py-1.5 text-xs font-medium transition-colors ${
                contentFilter === id
                  ? 'bg-white/[0.09] text-foreground'
                  : 'text-muted-foreground hover:text-foreground'
              }`}
            >
              {label}
            </button>
          ))}
        </div>

        <select
          value={selectedFolder ?? ''}
          onChange={(event) => setSelectedFolder(event.target.value || null)}
          className={filterSelectClass}
          aria-label="Filter by folder"
        >
          <option value="">All folders</option>
          {folders.map((folder) => (
            <option key={folder.id} value={folder.id}>
              {folder.name}
            </option>
          ))}
        </select>

        <select
          value={sourceApp ?? ''}
          onChange={(event) => setSourceApp(event.target.value || null)}
          className={filterSelectClass}
          aria-label="Filter by source app"
        >
          <option value="">All apps</option>
          {/* A previously chosen app can vanish from the list once its last
              clip is deleted. Keep it as an option so the control still shows
              what is actually being filtered on. */}
          {sourceApp && !sourceApps.some((app) => app.name === sourceApp) && (
            <option value={sourceApp}>{sourceApp} (0)</option>
          )}
          {sourceApps.map((app) => (
            <option key={app.name} value={app.name}>
              {app.name} ({app.count})
            </option>
          ))}
        </select>

        <select
          value={datePreset}
          onChange={(event) => setDatePreset(event.target.value as DatePreset)}
          className={filterSelectClass}
          aria-label="Filter by date"
        >
          {(Object.keys(DATE_PRESET_LABELS) as (keyof typeof DATE_PRESET_LABELS)[]).map((key) => (
            <option key={key} value={key}>
              {DATE_PRESET_LABELS[key]}
            </option>
          ))}
          <option value="custom">Custom range…</option>
        </select>
      </div>

      {datePreset === 'custom' && (
        <div className="flex shrink-0 items-center gap-2 border-b border-border px-4 py-2 text-xs text-muted-foreground">
          <label className="flex items-center gap-1.5">
            From
            <input
              type="date"
              value={customFrom}
              max={customTo || undefined}
              onChange={(event) => setCustomFrom(event.target.value)}
              className="rounded-md border border-white/[0.08] bg-transparent px-2 py-1 text-foreground outline-none"
            />
          </label>
          <label className="flex items-center gap-1.5">
            To
            <input
              type="date"
              value={customTo}
              min={customFrom || undefined}
              onChange={(event) => setCustomTo(event.target.value)}
              className="rounded-md border border-white/[0.08] bg-transparent px-2 py-1 text-foreground outline-none"
            />
          </label>
          {(customFrom || customTo) && (
            <button
              type="button"
              onClick={() => {
                setCustomFrom('');
                setCustomTo('');
              }}
              className="rounded p-1 hover:bg-white/10 hover:text-foreground"
              aria-label="Clear custom range"
            >
              <X size={13} />
            </button>
          )}
        </div>
      )}

      {selectionCount > 0 && (
        <div
          data-el="bulk-action-bar"
          className="flex shrink-0 flex-wrap items-center gap-1.5 border-b border-border bg-primary/[0.08] px-4 py-2"
        >
          <span className="mr-1 text-[11px] font-medium text-foreground">
            {selectionCount} of {totalClipCount.toLocaleString()} selected
          </span>
          <button type="button" className={bulkActionClass} onClick={handleBulkCopy}>
            <Copy size={12} />
            Copy
          </button>
          <button type="button" className={bulkActionClass} onClick={() => handleBulkPin(true)}>
            <Pin size={12} />
            Pin
          </button>
          <button type="button" className={bulkActionClass} onClick={() => handleBulkPin(false)}>
            <PinOff size={12} />
            Unpin
          </button>
          <select
            value=""
            onChange={(event) => {
              const value = event.target.value;
              if (value) handleBulkMove(value === '__none__' ? null : value);
            }}
            className={`${bulkActionClass} cursor-pointer`}
            aria-label="Move selected clips to a folder"
          >
            <option value="">Move to…</option>
            <option value="__none__">No folder</option>
            {folders.map((folder) => (
              <option key={folder.id} value={folder.id}>
                {folder.name}
              </option>
            ))}
          </select>
          <button
            type="button"
            className={`${bulkActionClass} text-destructive`}
            onClick={handleBulkDelete}
          >
            <Trash2 size={12} />
            Delete
          </button>
          <button
            type="button"
            onClick={clearSelection}
            className="ml-auto rounded-md px-2 py-1 text-[11px] text-muted-foreground hover:text-foreground"
          >
            Clear selection
          </button>
        </div>
      )}

      <main className="flex min-h-0 flex-1">
        <div className="min-h-0 w-[420px] shrink-0 border-r border-border">
          <ClipList
            clips={clips}
            isLoading={isLoading}
            hasMore={hasMore}
            resetToken={listResetToken}
            density={density}
            selectedClipId={selectedClipId}
            loadError={loadError}
            emptyTitle={emptyState.title}
            emptyDescription={emptyState.description}
            // Selection drives the preview pane here, so it follows clicks and
            // arrow keys only — not the pointer sweeping past on its way
            // somewhere else.
            selectOnHover={false}
            selectable
            checkedIds={selection.ids}
            onToggleSelect={handleToggleSelect}
            onSelectClip={setSelectedClipId}
            onPaste={setSelectedClipId}
            onCopy={handleCopy}
            revealedClips={revealed}
            onToggleReveal={handleToggleReveal}
            onTogglePin={handleTogglePin}
            onLoadMore={() => {
              if (hasMore && !isLoading) loadClips(true);
            }}
            onRetry={() => loadClips(false)}
          />
        </div>
        <div className="min-h-0 min-w-0 flex-1">
          <ClipPreview
            clip={selectedClip}
            onCopy={handleCopy}
            onCopyOcrText={handleCopyOcrText}
            onCopySelection={handleCopySelection}
            onOpenImage={handleOpenImage}
            onSaveOcrText={handleSaveOcrText}
            onRescanOcr={handleRescanOcr}
            onCopyText={handleCopySelection}
            onSaveText={handleSaveText}
            onSaveNotes={handleSaveNotes}
            onToggleHidden={handleToggleHidden}
            revealed={selectedClipId ? revealed.get(selectedClipId) : undefined}
            onToggleReveal={handleToggleReveal}
            onTogglePin={handleTogglePin}
            onDelete={handleDelete}
          />
        </div>
      </main>

      <footer className="flex h-9 shrink-0 items-center border-t border-border px-4 text-[10px] text-muted-foreground">
        <span>{totalClipCount.toLocaleString()} items</span>
        <div className="ml-auto flex items-center gap-3">
          <span>
            <kbd>Enter</kbd> Copy
          </span>
          <span>
            <kbd>Delete</kbd> Remove
          </span>
          <span>
            <kbd>Esc</kbd> Close
          </span>
        </div>
      </footer>
      <Toaster richColors position="bottom-center" theme={effectiveTheme} />
    </div>
  );
}
