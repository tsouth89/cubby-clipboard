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
import { useKeyboard } from './hooks/useKeyboard';
import { useTheme } from './hooks/useTheme';
import { useLanguage } from './hooks/useLanguage';
import { useSystemAccent } from './hooks/useSystemAccent';
import { useTranslation } from 'react-i18next';
import { Toaster, toast } from 'sonner';
import { generateDemoClips } from './debug/demoData';

function App() {
  const [clips, setClips] = useState<AppClipboardItem[]>([]);
  const [folders, setFolders] = useState<FolderItem[]>([]);
  const [selectedFolder, setSelectedFolder] = useState<string | null>(null);
  const [searchQuery, setSearchQuery] = useState('');
  const [contentFilter, setContentFilter] = useState<ContentFilter>('all');
  const [selectedClipId, setSelectedClipId] = useState<string | null>(null);
  const [clipListResetToken, setClipListResetToken] = useState(0);
  const [isLoading, setIsLoading] = useState(true);
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

  // Add Folder Modal State
  const [showAddFolderModal, setShowAddFolderModal] = useState(false);
  const [newFolderName, setNewFolderName] = useState('');

  const effectiveTheme = useTheme(theme);
  useSystemAccent();
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
    invoke<Settings>('get_settings')
      .then((s) => {
        setTheme(s.theme);
        setSettings(s);
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
    invoke<PasteContext>('get_paste_context').then(setPasteContext).catch(console.error);
  }, []);

  useEffect(() => {
    refreshPasteContext();
    window.addEventListener('focus', refreshPasteContext);
    return () => window.removeEventListener('focus', refreshPasteContext);
  }, [refreshPasteContext]);

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

        if (searchQuery.trim()) {
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
        console.error('Failed to load clips:', error);
        setLoadError(true);
        setHasMore(false);
      } finally {
        setIsLoading(false);
      }
    },
    [clips.length]
  );

  const loadFolders = useCallback(async () => {
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
    const unlistenClipboard = listen('clipboard-change', () => {
      refreshCurrentFolder();
      loadFolders(); // Refresh folders to get updated counts
      refreshTotalCount(); // Refresh total count
    });

    return () => {
      unlistenClipboard.then((unlisten) => {
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

  const handlePaste = async (clipId: string, plainText: boolean = false) => {
    try {
      const clip = clips.find((c) => c.id === clipId);
      if (!clip) return;
      if (plainText && clip.clip_type === 'image') return;

      await invoke('paste_clip', { id: clipId, plainText });
    } catch (error) {
      console.error('Failed to paste clip:', error);
      toast.error('Failed to paste clip');
    }
  };

  const handleCopy = async (clipId: string, plainText: boolean = false) => {
    try {
      const clip = clips.find((c) => c.id === clipId);
      if (!clip) return;
      if (plainText && clip.clip_type === 'image') return;

      await invoke('copy_clip', { id: clipId, plainText });

      toast.success(t('common.copied'));
    } catch (error) {
      console.error('Failed to copy clip:', error);
      toast.error(t('notifications.copyFailed'));
    }
  };

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
        if (contentFilter === 'text') return clip.clip_type !== 'image';
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
      if (contextMenu || clearRequest || showAddFolderModal) return;
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
  }, [clearRequest, contextMenu, showAddFolderModal]);

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
    if (!selectedClip || selectedClip.clip_type === 'image') return;
    handlePaste(selectedClipId, true);
  }, [selectedClipId, visibleClips, handlePaste]);

  const handleCopySelected = useCallback(() => {
    if (selectedClipId) {
      handleCopy(selectedClipId);
    }
  }, [selectedClipId, handleCopy]);

  useKeyboard({
    disabled: Boolean(contextMenu || clearRequest || showAddFolderModal),
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
                          {
                            label: t('contextMenu.pastePlainText'),
                            disabled: clip?.clip_type === 'image',
                            onClick: () => handlePaste(contextMenu.itemId, true),
                          },
                          {
                            label: t('contextMenu.copy'),
                            onClick: () => handleCopy(contextMenu.itemId),
                          },
                          {
                            label: t('contextMenu.copyPlainText'),
                            disabled: clip?.clip_type === 'image',
                            onClick: () => handleCopy(contextMenu.itemId, true),
                          },
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
              {selectedClipId &&
                visibleClips.find((clip) => clip.id === selectedClipId)?.clip_type !== 'image' && (
                  <span>
                    <kbd>Shift</kbd>
                    <kbd>Enter</kbd> Plain
                  </span>
                )}
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
