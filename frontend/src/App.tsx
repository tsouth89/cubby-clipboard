import { useEffect, useState, useCallback, useMemo, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { WebviewWindow } from '@tauri-apps/api/webviewWindow';
import { ClipboardItem as AppClipboardItem, FolderItem, PasteContext, Settings } from './types';
import { ClipList } from './components/ClipList';
import { ContentFilter, FlyoutHeader } from './components/FlyoutHeader';
import { ContextMenu } from './components/ContextMenu';
import { FolderModal } from './components/FolderModal';
import { ConfirmDialog } from './components/ConfirmDialog';
import { WelcomeOverlay } from './components/WelcomeOverlay';
import { useKeyboard } from './hooks/useKeyboard';
import { useTheme } from './hooks/useTheme';
import { useLanguage } from './hooks/useLanguage';
import { useSystemAccent } from './hooks/useSystemAccent';
import { useUpdater } from './hooks/useUpdater';
import { useTranslation } from 'react-i18next';
import { Toaster, toast } from 'sonner';
import { generateDemoClips } from './debug/demoData';

const assetCaptureEnabled = import.meta.env.DEV && import.meta.env.VITE_CUBBY_ASSET_CAPTURE === '1';

// Shown when the user tries to paste/copy the full image of a clip whose
// full-resolution blob was dropped by retention (SOU-244). Its thumbnail and
// recognized text remain, so the message points at what is still available.
const IMAGE_EXPIRED_MESSAGE =
  "This screenshot's full image expired. Only its recognized text remains.";

function App() {
  const [clips, setClips] = useState<AppClipboardItem[]>(() =>
    assetCaptureEnabled ? generateDemoClips().map((clip) => ({ ...clip, ocr_match: null })) : []
  );
  const [folders, setFolders] = useState<FolderItem[]>([]);
  const [selectedFolder, setSelectedFolder] = useState<string | null>(null);
  const [searchQuery, setSearchQuery] = useState('');
  const [contentFilter, setContentFilter] = useState<ContentFilter>('all');
  const [selectedClipId, setSelectedClipId] = useState<string | null>(null);
  const [clipListResetToken, setClipListResetToken] = useState(0);
  const [isLoading, setIsLoading] = useState(!assetCaptureEnabled);
  const [loadError, setLoadError] = useState(false);
  const [hasMore, setHasMore] = useState(true);
  const [theme, setTheme] = useState('system');
  const [settings, setSettings] = useState<Settings | null>(null);
  const [pasteContext, setPasteContext] = useState<PasteContext | null>(null);
  const [contextMenu, setContextMenu] = useState<{
    type: 'card' | 'folder' | 'history';
    x: number;
    y: number;
    itemId: string;
  } | null>(null);
  const [clearRequest, setClearRequest] = useState<'unpinned' | 'all' | null>(null);
  const [isClearing, setIsClearing] = useState(false);
  const [showWelcome, setShowWelcome] = useState(false);

  // Add Folder Modal State
  const [showAddFolderModal, setShowAddFolderModal] = useState(false);
  const [newFolderName, setNewFolderName] = useState('');

  const effectiveTheme = useTheme(theme);
  useSystemAccent();
  useUpdater();
  useLanguage(settings?.language);
  const { t } = useTranslation();
  const hasRoundedCorners = settings?.round_corners ?? true;
  const density = settings?.density ?? 'comfortable';
  const windowEffect =
    settings?.mica_effect === 'clear'
      ? 'solid'
      : settings?.mica_effect === 'mica_alt' || settings?.mica_effect === 'auto'
        ? 'acrylic'
        : settings?.mica_effect || 'solid';
  const windowSurface =
    windowEffect === 'solid'
      ? 'bg-[#171719]'
      : windowEffect === 'acrylic'
        ? 'bg-background/[0.58]'
        : 'bg-background/[0.08]';
  const windowBorder =
    windowEffect === 'acrylic'
      ? 'border-white/[0.14]'
      : windowEffect === 'mica'
        ? 'border-white/[0.10]'
        : 'border-white/[0.09]';
  const windowGeometry = hasRoundedCorners ? 'p-2' : 'p-0';
  const windowShape = hasRoundedCorners
    ? 'rounded-[10px] shadow-[0_24px_80px_rgba(0,0,0,0.48),0_6px_24px_rgba(0,0,0,0.32)]'
    : 'rounded-none shadow-none';

  const appWindow = getCurrentWindow();
  const selectedFolderRef = useRef(selectedFolder);
  selectedFolderRef.current = selectedFolder;
  const loadPerfIdRef = useRef(0);
  const perfLogEnabled =
    typeof window !== 'undefined' &&
    (window.location.hostname === 'localhost' || window.location.hostname === '127.0.0.1');

  useEffect(() => {
    if (assetCaptureEnabled) {
      setTheme('dark');
      setSettings({
        max_items: 500,
        auto_delete_days: 0,
        startup_with_windows: false,
        show_in_taskbar: false,
        hotkey: 'Win+V',
        replace_win_v: true,
        theme: 'dark',
        mica_effect: 'clear',
        round_corners: true,
        float_above_taskbar: true,
        density: 'comfortable',
        remote_paste_mode: 'copy_then_paste',
        ignore_ghost_clips: true,
        skip_sensitive: true,
        skip_likely_secrets: false,
        has_completed_onboarding: true,
      });
      return;
    }

    invoke<Settings>('get_settings')
      .then((s) => {
        setTheme(s.theme);
        setSettings(s);
        if (!s.has_completed_onboarding) {
          setShowWelcome(true);
        }
      })
      .catch(console.error);

    // Listen for setting changes from the settings window
    const unlisten = listen<Settings>('settings-changed', (event) => {
      setTheme(event.payload.theme);
      setSettings(event.payload);
    });

    // Debug only: load demo clips / restore actual data when triggered from settings
    const unlistenDemo = import.meta.env.DEV
      ? Promise.all([
          listen('load-demo-data', () => {
            setClips(generateDemoClips());
            setHasMore(false);
          }),
          listen('restore-actual-data', () => {
            loadClips(selectedFolderRef.current, false, '');
          }),
        ])
      : Promise.resolve([() => {}, () => {}]);

    return () => {
      unlisten.then((f) => f());
      unlistenDemo.then((fs) => fs.forEach((f) => f()));
    };
  }, []);

  const refreshPasteContext = useCallback(() => {
    if (assetCaptureEnabled) {
      setPasteContext({ target_kind: 'standard', remote_paste_mode: 'copy_then_paste' });
      return;
    }
    invoke<PasteContext>('get_paste_context').then(setPasteContext).catch(console.error);
  }, []);

  useEffect(() => {
    refreshPasteContext();
    window.addEventListener('focus', refreshPasteContext);
    return () => window.removeEventListener('focus', refreshPasteContext);
  }, [refreshPasteContext]);

  // Clear the search and any active filters whenever the flyout closes, so each
  // time Cubby opens it starts fresh instead of showing a stale, filtered list.
  // The window hides on blur, so focus loss is the reliable "closed" signal for
  // every dismissal path (Esc, click-away, and post-paste hide).
  useEffect(() => {
    const unlisten = appWindow.onFocusChanged(({ payload: focused }) => {
      if (!focused) {
        setSearchQuery('');
        setContentFilter('all');
        setSelectedFolder(null);
        // Reset selection and scroll the list back to the top, so reopening
        // always shows your most-recent copy instead of wherever you'd scrolled.
        setSelectedClipId(null);
        setClipListResetToken((prev) => prev + 1);
        // Dismiss any transient overlays so reopening lands on the clean list.
        setContextMenu(null);
        setClearRequest(null);
        setShowAddFolderModal(false);
        setNewFolderName('');
      }
    });
    return () => {
      unlisten.then((dispose) => dispose()).catch(() => undefined);
    };
  }, [appWindow]);

  const openSettings = useCallback(async () => {
    // Check if settings window already exists
    const existingWin = await WebviewWindow.getByLabel('settings');
    if (existingWin) {
      try {
        await invoke('focus_window', { label: 'settings' });
      } catch (e) {
        console.error('Failed to focus settings window:', e);
        // Fallback to JS API if command fails (though command is preferred)
        await existingWin.unminimize();
        await existingWin.show();
        await existingWin.setFocus();
      }
      return;
    }

    const settingsWin = new WebviewWindow('settings', {
      url: 'index.html?window=settings',
      title: 'Settings',
      width: 800,
      height: 700,
      resizable: true,
      decorations: false, // We have our own title bar in SettingsPanel
      transparent: false,
      center: true,
    });

    settingsWin.once('tauri://created', function () {});

    settingsWin.once('tauri://error', function (e) {
      console.error('Error creating settings window', e);
    });
  }, []);

  const handleDismissWelcome = useCallback(() => {
    setShowWelcome(false);
    setSettings((prev) => (prev ? { ...prev, has_completed_onboarding: true } : prev));
    invoke('complete_onboarding').catch(console.error);
  }, []);

  const loadClips = useCallback(
    async (folderId: string | null, append: boolean = false, searchQuery: string = '') => {
      const perfId = ++loadPerfIdRef.current;
      const loadStart = perfLogEnabled ? performance.now() : 0;
      let invokeStart = 0;
      let invokeEnd = 0;

      try {
        setIsLoading(true);
        setLoadError(false);

        const currentOffset = append ? clips.length : 0;

        let data: AppClipboardItem[];

        if (assetCaptureEnabled) {
          const query = searchQuery.trim().toLocaleLowerCase();
          data = generateDemoClips()
            .filter((clip) => {
              if (!query) return true;
              const searchable = [
                clip.content,
                clip.preview,
                clip.ocr_match?.before,
                clip.ocr_match?.matched,
                clip.ocr_match?.after,
              ]
                .filter(Boolean)
                .join(' ')
                .toLocaleLowerCase();
              return searchable.includes(query);
            })
            .map((clip) => ({ ...clip, ocr_match: query ? clip.ocr_match : null }));
          invokeStart = performance.now();
          invokeEnd = invokeStart;
        } else if (searchQuery.trim()) {
          if (perfLogEnabled) invokeStart = performance.now();
          data = await invoke<AppClipboardItem[]>('search_clips', {
            query: searchQuery,
            filterId: folderId,
            limit: 20,
            offset: currentOffset,
          });
          if (perfLogEnabled) invokeEnd = performance.now();
        } else {
          if (perfLogEnabled) invokeStart = performance.now();
          data = await invoke<AppClipboardItem[]>('get_clips', {
            filterId: folderId,
            limit: 20,
            offset: currentOffset,
            previewOnly: true,
          });
          if (perfLogEnabled) invokeEnd = performance.now();
        }

        const imageCount = perfLogEnabled
          ? data.filter((item) => item.clip_type === 'image').length
          : 0;
        const totalContentChars = perfLogEnabled
          ? data.reduce((sum, item) => sum + (item.content?.length ?? 0), 0)
          : 0;
        const imageContentChars = perfLogEnabled
          ? data
              .filter((item) => item.clip_type === 'image')
              .reduce((sum, item) => sum + (item.content?.length ?? 0), 0)
          : 0;

        // A newer load supersedes this one (e.g. the reset when the flyout
        // closes starts an unfiltered load while a filtered one is in flight).
        // Discard the stale result so it can't overwrite the current view.
        if (perfId !== loadPerfIdRef.current) return;

        if (append) {
          setClips((prev) => {
            return [...prev, ...data];
          });
        } else {
          setClips(data);
        }

        // If we got fewer than limit, no more clips
        setHasMore(data.length === 20);

        if (perfLogEnabled) {
          const stateQueuedAt = performance.now();
          requestAnimationFrame(() => {
            requestAnimationFrame(() => {
              const paintedAt = performance.now();
              console.info('[perf][loadClips]', {
                id: perfId,
                folderId: folderId ?? 'all',
                append,
                hasSearch: Boolean(searchQuery.trim()),
                offset: currentOffset,
                itemCount: data.length,
                imageCount,
                totalContentChars,
                imageContentChars,
                invokeMs: Number((invokeEnd - invokeStart).toFixed(1)),
                queueToPaintMs: Number((paintedAt - stateQueuedAt).toFixed(1)),
                totalMs: Number((paintedAt - loadStart).toFixed(1)),
              });
            });
          });
        }
      } catch (error) {
        if (perfId !== loadPerfIdRef.current) return;
        console.error('Failed to load clips:', error);
        setLoadError(true);
        setHasMore(false);
      } finally {
        if (perfId === loadPerfIdRef.current) setIsLoading(false);
      }
    },
    [clips.length]
  );

  const loadFolders = useCallback(async () => {
    if (assetCaptureEnabled) {
      setFolders([]);
      return;
    }
    try {
      const data = await invoke<FolderItem[]>('get_folders');

      setFolders(data);
    } catch (error) {
      console.error('Failed to load folders:', error);
    }
  }, []);

  const refreshCurrentFolder = useCallback(() => {
    loadClips(selectedFolderRef.current, false, searchQuery);
  }, [loadClips, searchQuery]);

  const handleSearch = useCallback((query: string) => {
    setSearchQuery(query);
  }, []);

  const handleSelectFolder = useCallback((folderId: string | null) => {
    // Reset view-level selection state whenever user switches/re-clicks folders.
    setSelectedClipId(null);
    setClipListResetToken((prev) => prev + 1);
    setSelectedFolder(folderId);
  }, []);

  useEffect(() => {
    loadFolders();
    if (searchQuery.trim()) {
      loadClips(selectedFolder, false, searchQuery);
    } else {
      loadClips(selectedFolder);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selectedFolder, searchQuery]);

  // Total History Count
  const [totalClipCount, setTotalClipCount] = useState(0);

  const refreshTotalCount = useCallback(async () => {
    if (assetCaptureEnabled) {
      setTotalClipCount(generateDemoClips().length);
      return;
    }
    try {
      const count = await invoke<number>('get_clipboard_history_size');
      setTotalClipCount(count);
    } catch (e) {
      console.error('Failed to get history size', e);
    }
  }, []);

  useEffect(() => {
    refreshTotalCount();
  }, [refreshTotalCount]);

  useEffect(() => {
    if (!assetCaptureEnabled) return;

    const query = 'clipboard service unavailable';
    const timers: number[] = [];
    let typingTimer: number | null = null;

    const runSearch = () => {
      setSearchQuery('');
      const input = document.querySelector<HTMLInputElement>('[data-el="search-input"]');
      input?.focus();
      let index = 0;
      typingTimer = window.setInterval(() => {
        index += 1;
        setSearchQuery(query.slice(0, index));
        if (index >= query.length && typingTimer !== null) {
          window.clearInterval(typingTimer);
          typingTimer = null;
          timers.push(window.setTimeout(runSearch, 2800));
        }
      }, 72);
    };

    timers.push(window.setTimeout(runSearch, 1800));

    return () => {
      timers.forEach(window.clearTimeout);
      if (typingTimer !== null) window.clearInterval(typingTimer);
    };
  }, []);

  useEffect(() => {
    const unlistenClipboard = listen('clipboard-change', () => {
      refreshCurrentFolder();
      loadFolders(); // Refresh folders to get updated counts
      refreshTotalCount(); // Refresh total count
    });

    // When a screenshot finishes OCR in the background, surface its "paste text"
    // affordance on the already-visible card instead of only after a reload.
    const unlistenOcr = listen<string>('ocr-completed', (event) => {
      const clipId = event.payload;
      setClips((prev) =>
        prev.map((clip) => (clip.id === clipId ? { ...clip, has_ocr_text: true } : clip))
      );
    });

    return () => {
      unlistenClipboard.then((unlisten) => {
        if (typeof unlisten === 'function') unlisten();
      });
      unlistenOcr.then((unlisten) => {
        if (typeof unlisten === 'function') unlisten();
      });
    };
  }, [refreshCurrentFolder, loadFolders, refreshTotalCount]);

  const handleDelete = async (clipId: string | null) => {
    if (!clipId) return;
    const deletedIndex = visibleClips.findIndex((clip) => clip.id === clipId);
    const remainingVisibleClips = visibleClips.filter((clip) => clip.id !== clipId);
    const nextSelection =
      deletedIndex < 0
        ? (remainingVisibleClips[0]?.id ?? null)
        : (remainingVisibleClips[Math.min(deletedIndex, remainingVisibleClips.length - 1)]?.id ??
          null);
    try {
      // Cubby has no trash/recovery surface. Delete must therefore remove the
      // persisted payload immediately instead of leaving a hidden soft-delete.
      await invoke('delete_clip', { id: clipId, hardDelete: true });
      setClips((currentClips) => currentClips.filter((clip) => clip.id !== clipId));
      setSelectedClipId(nextSelection);
      // Refresh counts
      loadFolders();
      refreshTotalCount();
      toast.success(t('notifications.clipDeleted'));
    } catch (error) {
      console.error('Failed to delete clip:', error);
      toast.error(t('notifications.clipDeleteFailed'));
    }
  };

  const handlePaste = useCallback(
    async (clipId: string, plainText: boolean = false) => {
      try {
        const clip = clips.find((c) => c.id === clipId);
        if (!clip) return;
        if (plainText && clip.clip_type === 'image') return;
        // The full-resolution image is gone (dropped by retention); its OCR text
        // is still one keystroke away via "paste recognized text". Don't hand
        // back the low-res thumbnail as if it were the original.
        if (clip.clip_type === 'image' && clip.image_expired) {
          toast.error(IMAGE_EXPIRED_MESSAGE);
          return;
        }

        await invoke('paste_clip', { id: clipId, plainText });
      } catch (error) {
        console.error('Failed to paste clip:', error);
        toast.error('Failed to paste clip');
      }
    },
    [clips]
  );

  const handleCopy = useCallback(
    async (clipId: string, plainText: boolean = false) => {
      try {
        const clip = clips.find((c) => c.id === clipId);
        if (!clip) return;
        if (plainText && clip.clip_type === 'image') return;
        if (clip.clip_type === 'image' && clip.image_expired) {
          toast.error(IMAGE_EXPIRED_MESSAGE);
          return;
        }

        await invoke('copy_clip', { id: clipId, plainText });

        toast.success(t('common.copied'));
      } catch (error) {
        console.error('Failed to copy clip:', error);
        toast.error(t('notifications.copyFailed'));
      }
    },
    [clips, t]
  );

  const handlePasteOcrText = useCallback(async (clipId: string) => {
    try {
      await invoke('paste_ocr_text', { id: clipId });
    } catch (error) {
      console.error('Failed to paste recognized text:', error);
      toast.error('Failed to paste recognized text');
    }
  }, []);

  const handleCopyOcrText = useCallback(
    async (clipId: string) => {
      try {
        await invoke('copy_ocr_text', { id: clipId });
        toast.success(t('common.copied'));
      } catch (error) {
        console.error('Failed to copy recognized text:', error);
        toast.error(t('notifications.copyFailed'));
      }
    },
    [t]
  );

  const handleTogglePin = useCallback(
    async (clipId: string | null) => {
      if (!clipId) return;
      try {
        const isPinned = await invoke<boolean>('toggle_clip_pin', { id: clipId });
        setClips((currentClips) =>
          currentClips
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
    },
    [setClips]
  );

  // Keyboard navigation handlers
  const visibleClips = useMemo(
    () =>
      clips.filter((clip) => {
        if (contentFilter === 'images') return clip.clip_type === 'image';
        if (contentFilter === 'text') return clip.clip_type === 'text';
        if (contentFilter === 'files')
          return clip.clip_type === 'file' || clip.clip_type === 'files';
        return true;
      }),
    [clips, contentFilter]
  );

  const emptyState = useMemo(() => {
    if (searchQuery.trim()) {
      return {
        title: t('clipList.noMatches'),
        description: t('clipList.noMatchesDesc'),
      };
    }
    if (selectedFolder) {
      return {
        title: t('clipList.emptyFolder'),
        description: t('clipList.emptyFolderDesc'),
      };
    }
    if (contentFilter === 'images') {
      return {
        title: t('clipList.noImages'),
        description: t('clipList.noImagesDesc'),
      };
    }
    if (contentFilter === 'text') {
      return {
        title: t('clipList.noText'),
        description: t('clipList.noTextDesc'),
      };
    }
    if (contentFilter === 'files') {
      return {
        title: t('clipList.noFiles'),
        description: t('clipList.noFilesDesc'),
      };
    }
    return {
      title: t('clipList.empty'),
      description: t('clipList.emptyDesc'),
    };
  }, [contentFilter, searchQuery, selectedFolder, t]);

  useEffect(() => {
    if (
      contentFilter !== 'all' &&
      clips.length > 0 &&
      visibleClips.length === 0 &&
      hasMore &&
      !isLoading
    ) {
      loadClips(selectedFolder, true, searchQuery);
    }
  }, [
    clips.length,
    contentFilter,
    hasMore,
    isLoading,
    loadClips,
    searchQuery,
    selectedFolder,
    visibleClips.length,
  ]);

  useEffect(() => {
    if (visibleClips.length === 0) {
      setSelectedClipId(null);
      return;
    }
    if (!selectedClipId || !visibleClips.some((clip) => clip.id === selectedClipId)) {
      setSelectedClipId(visibleClips[0].id);
    }
  }, [visibleClips, selectedClipId]);

  useEffect(() => {
    const focusSearchOnTyping = (event: KeyboardEvent) => {
      if (contextMenu || clearRequest || showAddFolderModal || showWelcome) return;
      const target = event.target as HTMLElement | null;
      const isEditing =
        target?.tagName === 'INPUT' || target?.tagName === 'TEXTAREA' || target?.isContentEditable;
      if (isEditing || event.ctrlKey || event.altKey || event.metaKey || event.key.length !== 1) {
        return;
      }

      const input = document.querySelector<HTMLInputElement>('[data-el="search-input"]');
      if (!input) return;
      event.preventDefault();
      input.focus();
      setSearchQuery((query) => `${query}${event.key}`);
    };

    document.addEventListener('keydown', focusSearchOnTyping);
    return () => document.removeEventListener('keydown', focusSearchOnTyping);
  }, [clearRequest, contextMenu, showAddFolderModal, showWelcome]);

  const handleNavigateUp = useCallback(() => {
    if (visibleClips.length === 0) return;

    if (!selectedClipId) {
      setSelectedClipId(visibleClips[0].id);
      return;
    }

    const currentIndex = visibleClips.findIndex((c) => c.id === selectedClipId);
    if (currentIndex > 0) {
      setSelectedClipId(visibleClips[currentIndex - 1].id);
    }
  }, [visibleClips, selectedClipId]);

  const handleNavigateDown = useCallback(() => {
    if (visibleClips.length === 0) return;

    if (!selectedClipId) {
      setSelectedClipId(visibleClips[0].id);
      return;
    }

    const currentIndex = visibleClips.findIndex((c) => c.id === selectedClipId);
    if (currentIndex < visibleClips.length - 1) {
      setSelectedClipId(visibleClips[currentIndex + 1].id);
    }
  }, [visibleClips, selectedClipId]);

  const handlePasteSelected = useCallback(() => {
    if (selectedClipId) {
      handlePaste(selectedClipId);
    }
  }, [selectedClipId, handlePaste]);

  const handlePasteSelectedAsPlainText = useCallback(() => {
    if (!selectedClipId) return;
    const selectedClip = visibleClips.find((clip) => clip.id === selectedClipId);
    if (!selectedClip) return;
    if (selectedClip.clip_type === 'image') {
      if (selectedClip.has_ocr_text) handlePasteOcrText(selectedClipId);
      return;
    }
    handlePaste(selectedClipId, true);
  }, [selectedClipId, visibleClips, handlePaste, handlePasteOcrText]);

  const handleCopySelected = useCallback(() => {
    if (selectedClipId) {
      handleCopy(selectedClipId);
    }
  }, [selectedClipId, handleCopy]);

  useKeyboard({
    disabled: Boolean(contextMenu || clearRequest || showAddFolderModal || showWelcome),
    onClose: () => {
      if (searchQuery) {
        setSearchQuery('');
        document.querySelector<HTMLInputElement>('[data-el="search-input"]')?.focus();
        return;
      }
      appWindow.hide();
    },
    onSearch: () => document.querySelector<HTMLInputElement>('[data-el="search-input"]')?.focus(),
    onDelete: () => handleDelete(selectedClipId),
    onPin: () => handleTogglePin(selectedClipId),
    onNavigateUp: handleNavigateUp,
    onNavigateDown: handleNavigateDown,
    onPaste: handlePasteSelected,
    onPastePlainText: handlePasteSelectedAsPlainText,
    onCopy: handleCopySelected,
  });

  const handleCreateFolder = async (name: string) => {
    try {
      await invoke('create_folder', { name, icon: null, color: null });
      await loadFolders();
    } catch (error) {
      console.error('Failed to create folder:', error);
    }
  };

  const handleMoveClip = async (clipId: string, folderId: string | null) => {
    try {
      await invoke('move_to_folder', { clipId, folderId });

      if (selectedFolder && folderId !== selectedFolder) {
        setClips((current) => current.filter((clip) => clip.id !== clipId));
        setSelectedClipId(null);
      } else {
        setClips((current) =>
          current.map((clip) => (clip.id === clipId ? { ...clip, folder_id: folderId } : clip))
        );
      }

      await loadFolders();
      toast.success(
        folderId
          ? `Moved to ${folders.find((folder) => folder.id === folderId)?.name ?? 'folder'}`
          : 'Removed from folder'
      );
    } catch (error) {
      console.error('Failed to move clip:', error);
      toast.error('Failed to move clip');
    }
  };

  const loadMore = useCallback(() => {
    if (hasMore && !isLoading) {
      loadClips(selectedFolder, true, searchQuery);
    }
  }, [hasMore, isLoading, selectedFolder, loadClips, searchQuery]);

  // New Folder Modal Rename Mode
  const [folderModalMode, setFolderModalMode] = useState<'create' | 'rename'>('create');
  const [editingFolderId, setEditingFolderId] = useState<string | null>(null);

  const handleContextMenu = useCallback(
    (e: React.MouseEvent, type: 'card' | 'folder', itemId: string) => {
      e.preventDefault();
      setContextMenu({
        type,
        x: e.clientX,
        y: e.clientY,
        itemId,
      });
    },
    []
  );

  const handleHistoryMenu = useCallback((event: React.MouseEvent<HTMLButtonElement>) => {
    const rect = event.currentTarget.getBoundingClientRect();
    setContextMenu({
      type: 'history',
      x: rect.right,
      y: rect.bottom + 4,
      itemId: '',
    });
  }, []);

  const handleCloseContextMenu = useCallback(() => {
    setContextMenu(null);
  }, []);

  // Updated Create Folder to handle Rename
  const handleCreateOrRenameFolder = async (name: string) => {
    if (folderModalMode === 'create') {
      await handleCreateFolder(name);
      toast.success(t('folders.folderCreated', { name }));
      setShowAddFolderModal(false);
      setNewFolderName('');
    } else if (folderModalMode === 'rename' && editingFolderId) {
      try {
        await invoke('rename_folder', { id: editingFolderId, name });
        await loadFolders();
        toast.success(t('folders.folderRenamed', { name }));
        setShowAddFolderModal(false);
        setNewFolderName('');
      } catch (error) {
        console.error('Failed to rename folder:', error);
        toast.error(t('notifications.folderRenameFailed'));
      }
    }
  };

  const handleDeleteFolder = async (folderId: string) => {
    if (!folderId) return;
    try {
      await invoke('delete_folder', { id: folderId });
      if (selectedFolder === folderId) {
        setSelectedFolder(null);
      }
      await loadFolders();
      refreshTotalCount();
      toast.success(t('folders.folderDeleted'));
    } catch (error) {
      console.error('Failed to delete folder:', error);
      toast.error(t('notifications.folderDeleteFailed'));
    }
  };

  const handleClearClips = async (mode: 'unpinned' | 'all') => {
    if (isClearing) return;
    setIsClearing(true);
    try {
      const deleted =
        mode === 'unpinned'
          ? await invoke<number>('clear_unpinned_clips')
          : (await invoke('clear_all_clips'), totalClipCount);

      setSelectedClipId(null);
      setClipListResetToken((token) => token + 1);
      await Promise.all([
        loadClips(selectedFolder, false, searchQuery),
        loadFolders(),
        refreshTotalCount(),
      ]);
      toast.success(
        mode === 'unpinned'
          ? t('notifications.clearUnpinnedSuccess', { count: deleted })
          : t('notifications.clearAllSuccess')
      );
      setClearRequest(null);
    } catch (error) {
      console.error('Failed to clear clipboard history:', error);
      toast.error(t('notifications.clearFailed'));
    } finally {
      setIsClearing(false);
    }
  };

  return (
    <div
      data-el="app-root"
      className={`relative h-screen w-full overflow-hidden bg-transparent ${windowGeometry}`}
    >
      <div
        data-el="app-window"
        className={`relative h-full w-full overflow-hidden border ${windowBorder} ${windowShape} ${windowSurface}`}
      >
        <div data-el="app-frame" className="flex h-full w-full flex-col font-sans text-foreground">
          {showWelcome && settings && <WelcomeOverlay onDismiss={handleDismissWelcome} />}

          {contextMenu && (
            <ContextMenu
              x={contextMenu.x}
              y={contextMenu.y}
              onClose={handleCloseContextMenu}
              options={
                contextMenu.type === 'history'
                  ? [
                      {
                        label: t('contextMenu.clearUnpinned'),
                        disabled: totalClipCount === 0,
                        onClick: () => setClearRequest('unpinned'),
                      },
                      {
                        label: t('contextMenu.clearAll'),
                        danger: true,
                        disabled: totalClipCount === 0,
                        onClick: () => setClearRequest('all'),
                      },
                    ]
                  : contextMenu.type === 'card'
                    ? (() => {
                        const clip = clips.find((item) => item.id === contextMenu.itemId);
                        return [
                          {
                            label: clip?.is_pinned ? 'Unpin' : 'Pin',
                            onClick: () => handleTogglePin(contextMenu.itemId),
                          },
                          {
                            label: t('contextMenu.paste'),
                            onClick: () => handlePaste(contextMenu.itemId),
                          },
                          ...(clip?.clip_type === 'image'
                            ? clip.has_ocr_text
                              ? [
                                  {
                                    label: t('contextMenu.pasteOcrText'),
                                    onClick: () => handlePasteOcrText(contextMenu.itemId),
                                  },
                                ]
                              : []
                            : [
                                {
                                  label: t('contextMenu.pastePlainText'),
                                  onClick: () => handlePaste(contextMenu.itemId, true),
                                },
                              ]),
                          {
                            label: t('contextMenu.copy'),
                            onClick: () => handleCopy(contextMenu.itemId),
                          },
                          ...(clip?.clip_type === 'image'
                            ? clip.has_ocr_text
                              ? [
                                  {
                                    label: t('contextMenu.copyOcrText'),
                                    onClick: () => handleCopyOcrText(contextMenu.itemId),
                                  },
                                ]
                              : []
                            : [
                                {
                                  label: t('contextMenu.copyPlainText'),
                                  onClick: () => handleCopy(contextMenu.itemId, true),
                                },
                              ]),
                          ...(clip?.folder_id
                            ? [
                                {
                                  label: 'Remove from folder',
                                  onClick: () => handleMoveClip(contextMenu.itemId, null),
                                },
                              ]
                            : []),
                          ...folders.map((folder) => ({
                            label: `Move to ${folder.name}`,
                            disabled: clip?.folder_id === folder.id,
                            onClick: () => handleMoveClip(contextMenu.itemId, folder.id),
                          })),
                          {
                            label: t('contextMenu.delete'),
                            danger: true,
                            onClick: () => handleDelete(contextMenu.itemId),
                          },
                        ];
                      })()
                    : [
                        {
                          label: t('contextMenu.rename'),
                          onClick: () => {
                            setFolderModalMode('rename');
                            setEditingFolderId(contextMenu.itemId);
                            const folder = folders.find((f) => f.id === contextMenu.itemId);
                            setNewFolderName(folder ? folder.name : '');
                            setShowAddFolderModal(true);
                          },
                        },
                        {
                          label: t('contextMenu.delete'),
                          danger: true,
                          onClick: () => handleDeleteFolder(contextMenu.itemId),
                        },
                      ]
              }
            />
          )}

          <ConfirmDialog
            isOpen={clearRequest !== null}
            title={
              clearRequest === 'all' ? t('clearHistory.allTitle') : t('clearHistory.unpinnedTitle')
            }
            message={
              clearRequest === 'all'
                ? t('clearHistory.allMessage')
                : t('clearHistory.unpinnedMessage')
            }
            confirmText={
              isClearing
                ? t('clearHistory.clearing')
                : clearRequest === 'all'
                  ? t('clearHistory.confirmAll')
                  : t('clearHistory.confirmUnpinned')
            }
            isBusy={isClearing}
            onConfirm={() => {
              if (clearRequest) void handleClearClips(clearRequest);
            }}
            onCancel={() => setClearRequest(null)}
          />

          <FlyoutHeader
            searchQuery={searchQuery}
            onSearchChange={handleSearch}
            contentFilter={contentFilter}
            onContentFilterChange={(filter) => {
              setContentFilter(filter);
              setSelectedClipId(null);
              setClipListResetToken((token) => token + 1);
            }}
            folders={folders}
            selectedFolder={selectedFolder}
            onSelectFolder={handleSelectFolder}
            onAddFolder={() => {
              setFolderModalMode('create');
              setNewFolderName('');
              setShowAddFolderModal(true);
            }}
            onOpenHistoryMenu={handleHistoryMenu}
            onOpenSettings={openSettings}
          />

          <main
            data-el="clip-list-area"
            className="no-scrollbar relative min-h-0 flex-1 overflow-hidden"
          >
            <ClipList
              clips={visibleClips}
              isLoading={isLoading || (visibleClips.length === 0 && hasMore)}
              hasMore={hasMore}
              resetToken={clipListResetToken}
              density={density}
              selectedClipId={selectedClipId}
              loadError={loadError}
              emptyTitle={emptyState.title}
              emptyDescription={emptyState.description}
              onSelectClip={setSelectedClipId}
              onPaste={handlePaste}
              onCopy={handleCopy}
              onTogglePin={handleTogglePin}
              onLoadMore={loadMore}
              onRetry={refreshCurrentFolder}
              onCardContextMenu={(e, clipId) => handleContextMenu(e, 'card', clipId)}
            />

            {/* Add/Rename Folder Modal Overlay */}
            <FolderModal
              isOpen={showAddFolderModal}
              mode={folderModalMode}
              initialName={newFolderName}
              onClose={() => {
                setShowAddFolderModal(false);
                setNewFolderName('');
              }}
              onSubmit={handleCreateOrRenameFolder}
            />
          </main>
          <footer className="drag-area flex h-9 shrink-0 items-center border-t border-white/[0.07] px-3 text-[10px] text-muted-foreground">
            <div className="flex min-w-0 items-center gap-2">
              <span>{totalClipCount.toLocaleString()} items</span>
              {pasteContext?.target_kind !== 'standard' && (
                <>
                  <span className="h-1 w-1 shrink-0 rounded-full bg-primary" />
                  <span className="truncate text-foreground/75">
                    {pasteContext?.target_kind === 'ninja'
                      ? pasteContext.remote_paste_mode === 'copy_then_paste'
                        ? t('pasteContext.ninjaCopy')
                        : t('pasteContext.ninjaKeystrokes')
                      : t('pasteContext.remote')}
                  </span>
                </>
              )}
            </div>
            <div className="ml-auto flex items-center gap-3">
              <span>
                <kbd>Enter</kbd>{' '}
                {pasteContext?.target_kind === 'ninja' &&
                pasteContext.remote_paste_mode === 'copy_then_paste'
                  ? t('pasteContext.copyAction')
                  : t('contextMenu.paste')}
              </span>
              {(() => {
                const selected = selectedClipId
                  ? visibleClips.find((clip) => clip.id === selectedClipId)
                  : undefined;
                if (!selected) return null;
                // Non-images paste as plain text; image results with OCR paste
                // their recognized text. Images without OCR have no Shift+Enter.
                if (selected.clip_type !== 'image') {
                  return (
                    <span>
                      <kbd>Shift</kbd>
                      <kbd>Enter</kbd> Plain
                    </span>
                  );
                }
                if (selected.has_ocr_text) {
                  return (
                    <span>
                      <kbd>Shift</kbd>
                      <kbd>Enter</kbd> Text
                    </span>
                  );
                }
                return null;
              })()}
              <span>
                <kbd>Esc</kbd> Close
              </span>
            </div>
          </footer>
          <Toaster richColors position="bottom-center" theme={effectiveTheme} />
        </div>
      </div>
    </div>
  );
}

export default App;
