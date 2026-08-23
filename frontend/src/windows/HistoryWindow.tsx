import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { CheckSquare, Copy, History, Pin, PinOff, Search, Trash2, X } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { Toaster, toast } from 'sonner';
import { ClipboardItem, FolderItem, Settings } from '../types';
import { ClipList } from '../components/ClipList';
import { ClipPreview } from '../components/ClipPreview';
import { ConfirmDialog } from '../components/ConfirmDialog';
import { ContentFilter } from '../components/FlyoutHeader';
import { useTheme } from '../hooks/useTheme';
import { useLanguage } from '../hooks/useLanguage';
import { useSystemAccent } from '../hooks/useSystemAccent';
import { useRevealedClips } from '../hooks/useRevealedClips';
import { collectBulkCopyText, type BulkCopyTextResult } from '../utils/bulkCopy';
import { shortcutsSuspended } from '../utils/flyoutSearch';
import { customRange, DATE_PRESET_LABELS, DatePreset, presetRange } from '../utils/dateRange';
import { folderSelectionAfterReload } from '../utils/folderSelection';
import { classifyKeyboardTarget } from '../utils/keyboardTarget';
import { clipListPageArgs, nextPageCursor } from '../utils/clipListPage';
import {
  clipLoadAnnouncesSuccess,
  clipLoadFailure,
  type ClipLoadResult,
  sidecarReloadFailure,
} from '../utils/clipLoadFailure';
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
  const { t } = useTranslation();
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
  const [bulkDeleteOpen, setBulkDeleteOpen] = useState(false);
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

  const { revealed, toggleReveal, forgetRevealed, updateRevealed, clearRevealed } =
    useRevealedClips();

  const clipsRef = useRef<ClipboardItem[]>(clips);
  clipsRef.current = clips;
  // Cursor for the next page, captured from the backend's own ordering.
  // Never read off clipsRef: a pin/unpin re-sorts that array in place.
  const pageCursorRef = useRef<string | null>(null);
  const selectedFolderRef = useRef(selectedFolder);
  selectedFolderRef.current = selectedFolder;
  // Guards against an older load landing after a newer one and restoring stale
  // results — the same discipline the flyout uses.
  const loadIdRef = useRef(0);
  // Which query the rows on screen answer. A failed replace only has to wipe
  // them when this no longer matches the load that failed — a same-filter
  // refresh (clipboard change, window focus, post-edit reload) leaves a
  // correct page.
  const visibleFilterKeyRef = useRef<string | null>(null);
  // A bulk Copy holds every selected body in memory while it runs; a second
  // click must not start a second fan-out over the same ids.
  const bulkCopyRunning = useRef(false);

  useEffect(() => {
    invoke<Settings>('get_settings').then(setSettings).catch(console.error);
    const unlisten = listen<Settings>('settings-changed', (event) => setSettings(event.payload));
    return () => {
      unlisten.then((dispose) => dispose()).catch(() => undefined);
    };
  }, []);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    getCurrentWindow()
      .onFocusChanged(({ payload: focused }) => {
        if (!focused) clearRevealed();
      })
      .then((fn) => {
        if (disposed) fn();
        else unlisten = fn;
      })
      .catch(() => undefined);
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [clearRevealed]);

  // Destructured so the load callback depends on the two stamps rather than a
  // fresh object identity each render, which would reload on every keystroke.
  const { from: dateFrom, to: dateTo } = dateRange;

  const loadClips = useCallback(
    async (append: boolean): Promise<ClipLoadResult> => {
      const loadId = ++loadIdRef.current;
      const page = clipListPageArgs(
        { loadedCount: clipsRef.current.length, cursorId: pageCursorRef.current },
        append,
        PAGE_SIZE
      );
      const query = searchQuery.trim();
      const filterKey = JSON.stringify([
        selectedFolder,
        query,
        contentFilter,
        dateFrom,
        dateTo,
        sourceApp,
      ]);

      try {
        setIsLoading(true);

        const data = query
          ? await invoke<ClipboardItem[]>('search_clips', {
              // The trimmed form, not the raw input: the index matches on the
              // normalized text, so a stray leading space would match nothing
              // while the box still looks like it holds a real query.
              query,
              filterId: selectedFolder,
              ...page,
              previewOnly: true,
              contentFilter,
              dateFrom,
              dateTo,
              sourceApp,
            })
          : await invoke<ClipboardItem[]>('get_clips', {
              filterId: selectedFolder,
              ...page,
              previewOnly: true,
              contentFilter,
              dateFrom,
              dateTo,
              sourceApp,
            });

        if (loadId !== loadIdRef.current) return 'superseded';
        setClips((previous) => (append ? [...previous, ...data] : data));
        pageCursorRef.current = nextPageCursor(data, append ? pageCursorRef.current : null);
        visibleFilterKeyRef.current = filterKey;
        setHasMore(data.length === PAGE_SIZE);
        setLoadError(false);
        return 'applied';
      } catch (error) {
        // Superseded is unknown, not success: a caller that toasts "deleted"
        // from this return would announce work the newer load has not applied.
        if (loadId !== loadIdRef.current) return 'superseded';
        console.error('Failed to load clips:', error);
        setHasMore(false);
        const failure = clipLoadFailure({
          append,
          visibleRowsStillApply: visibleFilterKeyRef.current === filterKey,
          hasVisibleClips: clipsRef.current.length > 0,
        });
        if (failure.clearList) {
          setClips([]);
          pageCursorRef.current = null;
          // The empty list now stands for this query, so retrying it is a
          // same-filter refresh rather than another filter change.
          visibleFilterKeyRef.current = filterKey;
        }
        if (!append) {
          setLoadError(true);
        }
        if (failure.notify) {
          toast.error(t(append ? 'clipList.loadMoreFailed' : 'clipList.refreshFailed'), {
            // One id, so a backend that keeps failing on every clipboard change
            // replaces its own message instead of stacking a wall of them.
            id: 'clip-load-failed',
          });
        }
        return 'failed';
      } finally {
        if (loadId === loadIdRef.current) setIsLoading(false);
      }
    },
    [contentFilter, dateFrom, dateTo, searchQuery, selectedFolder, sourceApp, t]
  );

  const loadFolders = useCallback(async () => {
    try {
      const data = await invoke<FolderItem[]>('get_folders');
      setFolders(data);
      const next = folderSelectionAfterReload(selectedFolderRef.current, data);
      if (next !== selectedFolderRef.current) {
        selectedFolderRef.current = next;
        setSelectedFolder(next);
      }
      return true;
    } catch (error) {
      console.error('Failed to load folders:', error);
      const failure = sidecarReloadFailure();
      if (failure.notify) {
        toast.error('Couldn’t refresh folders', { id: 'folder-load-failed' });
      }
      return false;
    }
  }, []);

  const loadSourceApps = useCallback(async () => {
    try {
      setSourceApps(await invoke<SourceAppCount[]>('get_source_apps'));
      return true;
    } catch (error) {
      console.error('Failed to load source apps:', error);
      const failure = sidecarReloadFailure();
      if (failure.notify) {
        toast.error('Couldn’t refresh apps', { id: 'source-apps-load-failed' });
      }
      return false;
    }
  }, []);

  const refreshTotalCount = useCallback(async () => {
    try {
      setTotalClipCount(await invoke<number>('get_clipboard_history_size'));
      return true;
    } catch (error) {
      console.error('Failed to get history size:', error);
      const failure = sidecarReloadFailure();
      if (failure.notify) {
        toast.error('Couldn’t refresh the clip count', { id: 'history-count-load-failed' });
      }
      return false;
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
      void (async () => {
        const previous = selectedFolderRef.current;
        await loadFolders();
        if (selectedFolderRef.current === previous) {
          loadClips(false);
        }
        refreshTotalCount();
        // A new clip can introduce an app, or move one up the list.
        loadSourceApps();
      })();
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
      void (async () => {
        const previous = selectedFolderRef.current;
        await loadFolders();
        if (selectedFolderRef.current === previous) {
          loadClips(false);
        }
        refreshTotalCount();
        // Deleting elsewhere can empty out an app, changing its count or dropping
        // it from the filter list entirely.
        loadSourceApps();
      })();
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

  /**
   * Returns the list reload's own result, not a boolean. loadClips reports
   * failure rather than throwing, so callers must check this before announcing
   * success: the rows on screen still describe the state before the bulk
   * change. Collapsing this to a boolean turns `superseded` into `failed`, and
   * a clipboard-change refresh supersedes this reload often enough that every
   * bulk action would report an error it did not have.
   */
  const afterBulkChange = useCallback(
    async (rowsStillApply = false): Promise<ClipLoadResult> => {
      clearSelection();
      // Delete and move take rows out of this query. Pin does not, so a failed
      // reload must keep the still-matching list instead of wiping it.
      if (!rowsStillApply) {
        visibleFilterKeyRef.current = null;
      }
      const [reloaded] = await Promise.all([
        loadClips(false),
        loadFolders(),
        refreshTotalCount(),
        loadSourceApps(),
      ]);
      return reloaded;
    },
    [clearSelection, loadClips, loadFolders, refreshTotalCount, loadSourceApps]
  );

  const handleBulkDelete = useCallback(async () => {
    if (selectionCount === 0) return;
    try {
      // Same contract as single-clip delete: Cubby has no trash, so the payload
      // goes immediately rather than leaving a hidden soft-delete.
      const deleted = await invoke<number>('delete_clips', {
        ids: selectedIds,
        hardDelete: true,
      });
      selectedIds.forEach((id) => forgetRevealed(id));
      // A failed reload already speaks: ClipList shows the error panel once
      // these rows are dropped. A superseded reload belongs to a newer load.
      // Either way this handler only withholds its success toast.
      if (!clipLoadAnnouncesSuccess(await afterBulkChange())) return;
      toast.success(deleted === 1 ? 'Deleted 1 clip' : `Deleted ${deleted} clips`);
    } catch (error) {
      console.error('Failed to delete clips:', error);
      toast.error('Failed to delete clips');
    }
  }, [afterBulkChange, forgetRevealed, selectedIds, selectionCount]);

  const handleBulkPin = useCallback(
    async (pinned: boolean) => {
      if (selectionCount === 0) return;
      try {
        await invoke<number>('set_clips_pinned', { ids: selectedIds, pinned });
        // Pin keeps the rows, so a failed reload shows the stale banner.
        if (!clipLoadAnnouncesSuccess(await afterBulkChange(true))) return;
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
        if (!clipLoadAnnouncesSuccess(await afterBulkChange())) return;
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
    // One plan at a time. Each body is a full decrypted clip, so a second click
    // during a large selection would double the fan-out against the same ids.
    if (bulkCopyRunning.current) return;
    bulkCopyRunning.current = true;
    try {
      const chosen = selectedIds
        .map((id) => clipsRef.current.find((clip) => clip.id === id))
        .filter((clip): clip is ClipboardItem => Boolean(clip));
      // List rows only carry previews and thumbnails. Load full text and image
      // OCR together without pulling full image blobs into the renderer.
      const { parts, skipped, hidden, failed } = await collectBulkCopyText(
        chosen,
        (ids) => invoke<BulkCopyTextResult[]>('get_bulk_copy_text', { ids }),
        { revealedIds: new Set(revealed.keys()) }
      );
      if (parts.length === 0) {
        toast.error(
          failed > 0
            ? 'Failed to load the selected clips'
            : hidden > 0
              ? 'Reveal the hidden clips before copying them'
              : 'Nothing to copy from the selected clips'
        );
        return;
      }
      await invoke('copy_selected_text', { text: parts.join('\n\n') });
      const notes: string[] = [];
      if (skipped > 0) notes.push(`${skipped} without text skipped`);
      if (hidden > 0) notes.push(`${hidden} hidden not copied`);
      if (failed > 0) notes.push(`${failed} failed to load`);
      toast.success(
        notes.length > 0
          ? `Copied ${parts.length} clips (${notes.join(', ')})`
          : `Copied ${parts.length} clips`
      );
    } catch (error) {
      console.error('Failed to copy clips:', error);
      toast.error('Failed to copy');
    } finally {
      bulkCopyRunning.current = false;
    }
  }, [revealed, selectedIds, selectionCount]);

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
        // Hidden rows still ship empty content after reload; keep the session
        // reveal in sync so leaving this clip and coming back does not revert.
        updateRevealed(clipId, { content: text, preview: text });
        // Same-filter reload: loadClips already shows the stale banner on
        // failure. A second toast here would talk over it.
        if (!clipLoadAnnouncesSuccess(await loadClips(false))) {
          return;
        }
        toast.success('Clip updated');
      } catch (error) {
        console.error('Failed to update the clip:', error);
        toast.error('Failed to update the clip');
        throw error;
      }
    },
    [loadClips, updateRevealed]
  );
  const handleSaveNotes = useCallback(
    async (clipId: string, notes: string) => {
      try {
        await invoke('set_clip_notes', { id: clipId, notes });
        // Cheap local update so the row's note appears immediately; a note also
        // changes what search matches, but not the current result set.
        setClips((current) =>
          current.map((clip) => (clip.id === clipId ? { ...clip, notes: notes || null } : clip))
        );
        // Hidden rows withhold notes; the reveal copy is what the pane reads.
        updateRevealed(clipId, { notes: notes || null });
      } catch (error) {
        console.error('Failed to save the note:', error);
        toast.error('Failed to save the note');
      }
    },
    [updateRevealed]
  );

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
      throw error;
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
        if (hidden) {
          // Blank the row immediately. loadClips is the only other writer, and
          // a failed same-filter reload would otherwise keep the plaintext on
          // screen with no error toast (SBS-1006).
          setClips((current) =>
            current.map((clip) =>
              clip.id === clipId
                ? { ...clip, is_hidden: true, content: '', preview: '', notes: null }
                : clip
            )
          );
        }
        // loadClips reports failure rather than throwing, so a stale list after
        // a failed reload would otherwise be announced as success.
        // Same-filter reload: a failure shows the stale banner over the rows,
        // the way handleSaveText already relies on. A toast here would be a
        // second channel for one failed reload.
        if (!clipLoadAnnouncesSuccess(await loadClips(false))) return;
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
        forgetRevealed(clipId);
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
    [forgetRevealed, loadFolders, refreshTotalCount]
  );

  const handleClose = useCallback(() => {
    getCurrentWindow()
      .close()
      .catch((error) => console.error('Failed to close the history window:', error));
  }, []);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      // While the bulk-delete confirm is up, every shortcut here is wrong:
      // Delete would hard-delete the previewed clip mid-question, Escape would
      // close the whole window, and Enter would preventDefault the dialog's own
      // buttons. App.tsx stands down the same way with useKeyboard's disabled
      // flag.
      if (shortcutsSuspended(event, bulkDeleteOpen)) return;
      const { isEditing, isInteractive, isSearch } = classifyKeyboardTarget(
        event.target,
        '[data-el="history-search-input"]'
      );

      if (event.key === 'Escape') {
        // Search is an INPUT, so isEditing is true there; Escape still clears
        // the query / closes. Notes and other fields must not close the window
        // even if they forget to stopPropagation (SBS-1008).
        if (isEditing && !isSearch) return;
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
      if ((event.key === 'ArrowUp' || event.key === 'ArrowDown') && !isInteractive) {
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
      if (isInteractive) return;
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
  }, [bulkDeleteOpen, handleClose, handleCopy, handleDelete, searchQuery, selectedClipId]);

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
            {selectionCount} of {clips.length.toLocaleString()} selected
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
            onClick={() => setBulkDeleteOpen(true)}
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
      <ConfirmDialog
        isOpen={bulkDeleteOpen}
        title={selectionCount === 1 ? 'Delete this clip?' : `Delete ${selectionCount} clips?`}
        message="Cubby has no trash. Deleted clips cannot be recovered."
        confirmText={selectionCount === 1 ? 'Delete clip' : `Delete ${selectionCount} clips`}
        onConfirm={() => {
          setBulkDeleteOpen(false);
          void handleBulkDelete();
        }}
        onCancel={() => setBulkDeleteOpen(false)}
      />
    </div>
  );
}
