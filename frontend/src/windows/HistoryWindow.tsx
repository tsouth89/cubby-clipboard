import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { History, Search, X } from 'lucide-react';
import { Toaster, toast } from 'sonner';
import { ClipboardItem, FolderItem, Settings } from '../types';
import { ClipList } from '../components/ClipList';
import { ClipPreview } from '../components/ClipPreview';
import { ContentFilter } from '../components/FlyoutHeader';
import { useTheme } from '../hooks/useTheme';
import { useLanguage } from '../hooks/useLanguage';
import { useSystemAccent } from '../hooks/useSystemAccent';

const PAGE_SIZE = 20;

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

  const effectiveTheme = useTheme(settings?.theme ?? 'system');
  useLanguage(settings?.language);
  // Each Tauri window is its own document, so this one has to apply the Windows
  // accent color itself.
  useSystemAccent();
  const density = settings?.density ?? 'comfortable';

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
            })
          : await invoke<ClipboardItem[]>('get_clips', {
              filterId: selectedFolder,
              limit: PAGE_SIZE,
              offset,
              previewOnly: true,
              contentFilter,
            });

        if (loadId !== loadIdRef.current) return;
        setClips((previous) => (append ? [...previous, ...data] : data));
        setHasMore(data.length === PAGE_SIZE);
      } catch (error) {
        if (loadId !== loadIdRef.current) return;
        console.error('Failed to load clips:', error);
        setLoadError(true);
        setHasMore(false);
      } finally {
        if (loadId === loadIdRef.current) setIsLoading(false);
      }
    },
    [contentFilter, searchQuery, selectedFolder]
  );

  const loadFolders = useCallback(async () => {
    try {
      setFolders(await invoke<FolderItem[]>('get_folders'));
    } catch (error) {
      console.error('Failed to load folders:', error);
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

  useEffect(() => {
    loadFolders();
    refreshTotalCount();
  }, [loadFolders, refreshTotalCount]);

  // Keep the window live while it sits open next to the flyout: a copy made
  // anywhere shows up here without a manual refresh.
  useEffect(() => {
    const unlistenClipboard = listen('clipboard-change', () => {
      loadClips(false);
      loadFolders();
      refreshTotalCount();
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
    };
    window.addEventListener('focus', refreshOnFocus);

    return () => {
      window.removeEventListener('focus', refreshOnFocus);
      unlistenClipboard.then((dispose) => dispose()).catch(() => undefined);
      unlistenOcr.then((dispose) => dispose()).catch(() => undefined);
    };
  }, [loadClips, loadFolders, refreshTotalCount]);

  const selectedClip = useMemo(
    () => clips.find((clip) => clip.id === selectedClipId) ?? null,
    [clips, selectedClipId]
  );

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

  const emptyState = searchQuery.trim()
    ? { title: 'No matches', description: 'Try a different search term.' }
    : selectedFolder
      ? { title: 'This folder is empty', description: 'Move clips here to keep them together.' }
      : contentFilter === 'images'
        ? { title: 'No images yet', description: 'Copy an image and it will show up here.' }
        : contentFilter === 'text'
          ? { title: 'No text clips yet', description: 'Copy some text and it will show up here.' }
          : { title: 'Nothing here yet', description: 'Anything you copy will appear in Cubby.' };

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
          className="h-9 max-w-[160px] shrink-0 rounded-md border border-white/[0.08] bg-transparent px-2 text-xs text-muted-foreground outline-none hover:text-foreground"
          aria-label="Filter by folder"
        >
          <option value="">All folders</option>
          {folders.map((folder) => (
            <option key={folder.id} value={folder.id}>
              {folder.name}
            </option>
          ))}
        </select>
      </div>

      <main className="flex min-h-0 flex-1">
        <div className="min-h-0 w-[420px] shrink-0 border-r border-border">
          <ClipList
            clips={clips}
            isLoading={isLoading}
            hasMore={hasMore}
            resetToken={0}
            density={density}
            selectedClipId={selectedClipId}
            loadError={loadError}
            emptyTitle={emptyState.title}
            emptyDescription={emptyState.description}
            // Selection drives the preview pane here, so it follows clicks and
            // arrow keys only — not the pointer sweeping past on its way
            // somewhere else.
            selectOnHover={false}
            onSelectClip={setSelectedClipId}
            onPaste={setSelectedClipId}
            onCopy={handleCopy}
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
