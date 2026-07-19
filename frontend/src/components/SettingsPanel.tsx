import { Settings, FolderItem } from '../types';
import {
  X,
  Trash2,
  Plus,
  FolderOpen,
  Settings as SettingsIcon,
  Folder as FolderIcon,
  MoreHorizontal,
  Pause,
  Play,
  RefreshCw,
  ShieldCheck,
  Info,
  Github,
  Globe,
  ExternalLink,
  Lock,
  AlertTriangle,
  FlaskConical,
} from 'lucide-react';
import { useState, useEffect, useRef, type ReactNode } from 'react';
import { useTheme } from '../hooks/useTheme';
import { useTranslation } from 'react-i18next';
import { invoke } from '@tauri-apps/api/core';
import { emit } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { getVersion } from '@tauri-apps/api/app';
import { openUrl } from '@tauri-apps/plugin-opener';
import { check } from '@tauri-apps/plugin-updater';
import { relaunch } from '@tauri-apps/plugin-process';
import { toast } from 'sonner';
import { ConfirmDialog } from './ConfirmDialog';
import { Select } from './ui/Select';
import { useShortcutRecorder } from 'use-shortcut-recorder';
import { clsx } from 'clsx';

interface SettingsPanelProps {
  settings: Settings;
  onClose: () => void;
}

type Tab = 'general' | 'privacy' | 'folders' | 'about';

const GITHUB_URL = 'https://github.com/tsouth89/cubby-clipboard';
const WEBSITE_URL = 'https://cubbyclipboard.com';
const PRIVACY_URL = 'https://cubbyclipboard.com/privacy';

type DittoImportResult = {
  total: number;
  imported: number;
  duplicates: number;
  skipped_groups: number;
  skipped_images: number;
  skipped_empty: number;
  errors: string[];
  dry_run: boolean;
};

type OcrQueueStatus = {
  pending: number;
  processing: number;
  completed: number;
  failed: number;
  unavailable: number;
  paused: boolean;
};

type StorageUsage = {
  items: number;
  bytes: number;
};

function formatBytes(bytes: number): string {
  if (bytes <= 0) return '0 MB';
  if (bytes < 1024) return `${bytes} B`;
  const kb = bytes / 1024;
  if (kb < 1024) return `${Math.round(kb)} KB`;
  const mb = kb / 1024;
  if (mb < 1024) return `${mb < 10 ? mb.toFixed(1) : Math.round(mb)} MB`;
  return `${(mb / 1024).toFixed(1)} GB`;
}

function CubbyMark({ className }: { className?: string }) {
  return (
    <svg viewBox="0 0 64 64" className={className} aria-hidden="true">
      <path
        fill="#147ee8"
        d="M14 2h34c7.7 0 14 6.3 14 14v5H32c-6.6 0-12 5.4-12 12s5.4 12 12 12h30v3c0 7.7-6.3 14-14 14H14C6.3 62 0 55.7 0 48V16C0 8.3 6.3 2 14 2Z"
      />
      <rect x="42" y="25" width="20" height="16" rx="8" fill="#32aeb1" />
    </svg>
  );
}

function PaneHeader({ title, subtitle }: { title: string; subtitle: string }) {
  return (
    <div>
      <h1 className="text-xl font-semibold tracking-tight">{title}</h1>
      <p className="mt-1 text-[13px] text-muted-foreground">{subtitle}</p>
    </div>
  );
}

function SectionLabel({ children }: { children: ReactNode }) {
  return (
    <p className="mb-2 ml-1 text-[11px] font-semibold uppercase tracking-[0.09em] text-muted-foreground">
      {children}
    </p>
  );
}

function SettingCard({ children }: { children: ReactNode }) {
  return (
    <div className="divide-y divide-border overflow-hidden rounded-xl border border-border bg-card">
      {children}
    </div>
  );
}

function Row({
  title,
  desc,
  control,
  children,
}: {
  title?: ReactNode;
  desc?: ReactNode;
  control?: ReactNode;
  children?: ReactNode;
}) {
  if (children) {
    return (
      <div className="px-4 py-3.5">
        {(title || desc) && (
          <div className="mb-3">
            {title && <div className="text-sm font-medium">{title}</div>}
            {desc && <p className="mt-0.5 text-xs leading-snug text-muted-foreground">{desc}</p>}
          </div>
        )}
        {children}
      </div>
    );
  }
  return (
    <div className="flex items-center gap-4 px-4 py-3.5">
      <div className="min-w-0 flex-1">
        {title && <div className="text-sm font-medium">{title}</div>}
        {desc && <p className="mt-0.5 text-xs leading-snug text-muted-foreground">{desc}</p>}
      </div>
      {control && <div className="flex-shrink-0">{control}</div>}
    </div>
  );
}

function Toggle({
  checked,
  onChange,
  disabled,
  label,
}: {
  checked: boolean;
  onChange: () => void;
  disabled?: boolean;
  label?: string;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      disabled={disabled}
      onClick={onChange}
      className={clsx(
        'relative h-6 w-11 flex-shrink-0 rounded-full transition-colors',
        checked ? 'bg-primary' : 'bg-accent',
        disabled && 'cursor-not-allowed opacity-40'
      )}
    >
      <span
        className={clsx(
          'absolute left-0.5 top-0.5 h-5 w-5 rounded-full bg-white shadow-sm transition-transform',
          checked ? 'translate-x-5' : 'translate-x-0'
        )}
      />
    </button>
  );
}

function Segmented({
  value,
  onChange,
  options,
}: {
  value: string;
  onChange: (value: string) => void;
  options: { value: string; label: string }[];
}) {
  return (
    <div className="inline-flex gap-0.5 rounded-lg border border-border bg-accent/40 p-0.5">
      {options.map((option) => (
        <button
          key={option.value}
          type="button"
          onClick={() => onChange(option.value)}
          className={clsx(
            'rounded-md px-3 py-1.5 text-xs font-medium transition-colors',
            value === option.value
              ? 'bg-primary text-primary-foreground'
              : 'text-muted-foreground hover:text-foreground'
          )}
        >
          {option.label}
        </button>
      ))}
    </div>
  );
}

export function SettingsPanel({ settings: initialSettings, onClose }: SettingsPanelProps) {
  const [activeTab, setActiveTab] = useState<Tab>('general');
  const [settings, setSettings] = useState<Settings>(initialSettings);
  const settingsRef = useRef<Settings>(initialSettings);
  const settingsSaveQueue = useRef<Promise<void>>(Promise.resolve());
  const [_historySize, setHistorySize] = useState<number>(0);
  const [isRecordingMode, setIsRecordingMode] = useState(false);
  const [checkingUpdate, setCheckingUpdate] = useState(false);
  // Folder Management State
  const [folders, setFolders] = useState<FolderItem[]>([]);
  const [newFolderName, setNewFolderName] = useState('');
  const [editingFolderId, setEditingFolderId] = useState<string | null>(null);
  const [renameValue, setRenameValue] = useState('');

  // Apply theme immediately when settings.theme changes
  useTheme(settings.theme);

  // i18n hook
  const { i18n, t } = useTranslation();

  // Generic handler for immediate settings updates
  const updateSettings = (updates: Partial<Settings>) => {
    settingsSaveQueue.current = settingsSaveQueue.current
      .catch(() => undefined)
      .then(async () => {
        const newSettings = { ...settingsRef.current, ...updates };
        try {
          await invoke('save_settings', { settings: newSettings });
          settingsRef.current = newSettings;
          setSettings(newSettings);
          await emit('settings-changed', newSettings);
          if ('float_above_taskbar' in updates) {
            await invoke('refresh_window');
          }

          const keys = Object.keys(updates);
          if (keys.length === 1) {
            const key = keys[0] as keyof Settings;
            const value = updates[key];
            if (key !== 'theme') {
              const label = key
                .split('_')
                .map((w) => w.charAt(0).toUpperCase() + w.slice(1))
                .join(' ');
              if (typeof value === 'boolean') {
                toast.success(`${label} was ${value ? 'enabled' : 'disabled'}`);
              } else {
                toast.success(`${label} updated`);
              }
            }
          } else if (keys.length > 1) {
            toast.success('Settings updated');
          }
        } catch (error) {
          console.error(`Failed to save settings:`, error);
          toast.error(String(error));
        }
      });

    return settingsSaveQueue.current;
  };

  const updateSetting = (key: keyof Settings, value: any) => {
    updateSettings({ [key]: value });
  };

  const handleThemeChange = (newTheme: string) => {
    updateSetting('theme', newTheme);
  };

  const handleLanguageChange = (newLanguage: string) => {
    updateSetting('language', newLanguage);
    // Change language immediately
    i18n.changeLanguage(newLanguage);
    localStorage.setItem('cubby_language', newLanguage);
  };

  // Use use-shortcut-recorder for recording (shows current keys held in real-time)
  const {
    shortcut,
    savedShortcut,
    startRecording: startRecordingLib,
    stopRecording: stopRecordingLib,
    clearLastRecording,
  } = useShortcutRecorder({
    minModKeys: 1, // Require at least one modifier
  });

  // Start recording mode
  const handleStartRecording = () => {
    setIsRecordingMode(true);
    startRecordingLib();
  };

  const [ignoredApps, setIgnoredApps] = useState<string[]>([]);
  const [newIgnoredApp, setNewIgnoredApp] = useState('');
  const [appVersion, setAppVersion] = useState('');
  const [dittoBusy, setDittoBusy] = useState(false);
  const [ocrStatus, setOcrStatus] = useState<OcrQueueStatus | null>(null);
  const [ocrActionBusy, setOcrActionBusy] = useState(false);
  const [storageUsage, setStorageUsage] = useState<StorageUsage | null>(null);
  const ocrRemaining = (ocrStatus?.pending ?? 0) + (ocrStatus?.processing ?? 0);
  const ocrFailures = (ocrStatus?.failed ?? 0) + (ocrStatus?.unavailable ?? 0);
  const ocrStatusLabel = !ocrStatus
    ? t('common.loading')
    : ocrStatus.paused
      ? t('settings.ocrPaused')
      : ocrRemaining > 0
        ? t('settings.ocrIndexing')
        : ocrFailures > 0
          ? t('settings.ocrNeedsAttention')
          : t('settings.ocrReady');

  // Confirmation Dialog State
  const [confirmDialog, setConfirmDialog] = useState({
    isOpen: false,
    title: '',
    message: '',
    action: async () => {},
  });

  const loadFolders = async () => {
    try {
      const data = await invoke<FolderItem[]>('get_folders');
      setFolders(data);
    } catch (error) {
      console.error('Failed to load folders:', error);
    }
  };

  const loadOcrStatus = async () => {
    try {
      setOcrStatus(await invoke<OcrQueueStatus>('get_ocr_queue_status'));
    } catch (error) {
      console.error('Failed to load OCR index status:', error);
    }
  };

  const loadStorageUsage = async () => {
    try {
      setStorageUsage(await invoke<StorageUsage>('get_storage_usage'));
    } catch (error) {
      console.error('Failed to load storage usage:', error);
    }
  };

  useEffect(() => {
    invoke<number>('get_clipboard_history_size').then(setHistorySize).catch(console.error);
    invoke<string[]>('get_ignored_apps').then(setIgnoredApps).catch(console.error);
    getVersion().then(setAppVersion).catch(console.error);
    loadFolders();
    loadOcrStatus();
    loadStorageUsage();
    const ocrStatusTimer = window.setInterval(loadOcrStatus, 3000);
    return () => window.clearInterval(ocrStatusTimer);
  }, []);

  const handleRetentionChange = async (value: string) => {
    await updateSettings({ auto_delete_days: Number(value), max_items: 0 });
    try {
      await invoke('apply_retention');
      await loadStorageUsage();
    } catch (error) {
      console.error('Failed to apply retention:', error);
    }
  };

  const handleOcrPauseToggle = async () => {
    if (!ocrStatus) return;
    setOcrActionBusy(true);
    try {
      await invoke('set_ocr_queue_paused', { paused: !ocrStatus.paused });
      await loadOcrStatus();
    } catch (error) {
      toast.error(String(error));
    } finally {
      setOcrActionBusy(false);
    }
  };

  const handleRetryOcr = async () => {
    setOcrActionBusy(true);
    try {
      const count = await invoke<number>('retry_failed_ocr');
      toast.success(t('settings.ocrRetryQueued', { count }));
      await loadOcrStatus();
    } catch (error) {
      toast.error(String(error));
    } finally {
      setOcrActionBusy(false);
    }
  };

  const handleCheckUpdates = async () => {
    setCheckingUpdate(true);
    try {
      const update = await check();
      if (update?.available) {
        toast(t('updater.available', { version: update.version }), {
          duration: Infinity,
          action: {
            label: t('updater.install'),
            onClick: () => {
              const toastId = toast.loading(t('updater.installing'));
              update
                .downloadAndInstall()
                .then(() => {
                  toast.success(t('updater.restarting'), { id: toastId });
                  return relaunch();
                })
                .catch((error) => {
                  console.error('Update install failed:', error);
                  toast.error(t('updater.failed'), { id: toastId });
                });
            },
          },
        });
      } else {
        toast.success(t('settings.upToDate'));
      }
    } catch (error) {
      console.error('Update check failed:', error);
      toast.error(t('updater.failed'));
    } finally {
      setCheckingUpdate(false);
    }
  };

  const handleAddIgnoredApp = async () => {
    if (!newIgnoredApp.trim()) return;
    try {
      await invoke('add_ignored_app', { appName: newIgnoredApp.trim() });
      setIgnoredApps((prev) => [...prev, newIgnoredApp.trim()].sort());
      setNewIgnoredApp('');
      toast.success(`Added ${newIgnoredApp.trim()} to ignored apps`);
    } catch (e) {
      toast.error(`Failed to add ignored app: ${e}`);
      console.error(e);
    }
  };

  const handleBrowseFile = async () => {
    try {
      const path = await invoke<string>('pick_file');
      const filename = path.split(/[\\/]/).pop() || path;
      setNewIgnoredApp(filename);
    } catch (e) {
      console.log('File picker cancelled or failed', e);
    }
  };

  const handleImportFromDitto = async () => {
    let dbPath: string;
    try {
      dbPath = await invoke<string>('pick_ditto_database');
    } catch {
      return; // picker cancelled
    }

    let preview: DittoImportResult;
    setDittoBusy(true);
    try {
      preview = await invoke<DittoImportResult>('import_from_ditto', { dbPath, dryRun: true });
    } catch (e) {
      toast.error(t('settings.dittoImportError', { error: String(e) }));
      return;
    } finally {
      setDittoBusy(false);
    }

    if (preview.imported === 0) {
      toast.info(
        preview.duplicates > 0
          ? t('settings.dittoAllDuplicates')
          : t('settings.dittoNothingToImport')
      );
      return;
    }

    setConfirmDialog({
      isOpen: true,
      title: t('settings.dittoImportTitle'),
      message: t('settings.dittoImportConfirm', { count: preview.imported }),
      action: async () => {
        setDittoBusy(true);
        try {
          const result = await invoke<DittoImportResult>('import_from_ditto', {
            dbPath,
            dryRun: false,
          });
          if (result.errors.length > 0) {
            toast.warning(
              t('settings.dittoImportPartial', {
                count: result.imported,
                failed: result.errors.length,
              })
            );
          } else {
            toast.success(t('settings.dittoImportSuccess', { count: result.imported }));
          }
        } catch (e) {
          toast.error(t('settings.dittoImportError', { error: String(e) }));
        } finally {
          setDittoBusy(false);
        }
      },
    });
  };

  const handleRemoveIgnoredApp = async (app: string) => {
    try {
      await invoke('remove_ignored_app', { appName: app });
      setIgnoredApps((prev) => prev.filter((a) => a !== app));
      toast.success(`Removed ${app} from ignored apps`);
    } catch (e) {
      toast.error(`Failed to remove ignored app: ${e}`);
      console.error(e);
    }
  };

  const handleRemoveDuplicates = async () => {
    try {
      const count = await invoke<number>('remove_duplicate_clips');
      toast.success(t('settings.removeDuplicatesSuccess', { count }));
      const newSize = await invoke<number>('get_clipboard_history_size');
      setHistorySize(newSize);
      loadStorageUsage();
    } catch (error) {
      console.error(error);
      toast.error(`Failed to remove duplicates: ${error}`);
    }
  };

  const confirmClearHistory = () => {
    setConfirmDialog({
      isOpen: true,
      title: t('settings.clearHistoryTitle'),
      message: t('settings.clearHistoryMessage'),
      action: async () => {
        try {
          await invoke('clear_all_clips');
          setHistorySize(0);
          loadStorageUsage();
          toast.success(t('settings.clearHistorySuccess'));
        } catch (error) {
          console.error('Failed to clear history:', error);
          toast.error(`Failed to clear history: ${error}`);
        }
      },
    });
  };

  // Folder Management Functions
  const handleCreateFolder = async () => {
    if (!newFolderName.trim()) return;
    try {
      await invoke('create_folder', { name: newFolderName.trim(), icon: null, color: null });
      setNewFolderName('');
      await loadFolders();
      toast.success('Folder created');
    } catch (e) {
      toast.error(`Failed to create folder: ${e}`);
    }
  };

  const handleDeleteFolder = async (id: string) => {
    try {
      await invoke('delete_folder', { id });
      await loadFolders();
      toast.success('Folder deleted');
    } catch (e) {
      toast.error(`Failed to delete folder: ${e}`);
    }
  };

  const startRenameFolder = (folder: FolderItem) => {
    setEditingFolderId(folder.id);
    setRenameValue(folder.name);
  };

  const saveRenameFolder = async () => {
    if (!editingFolderId || !renameValue.trim()) return;
    try {
      await invoke('rename_folder', { id: editingFolderId, name: renameValue.trim() });
      setEditingFolderId(null);
      setRenameValue('');
      await loadFolders();
      toast.success('Folder renamed');
    } catch (e) {
      toast.error(`Failed to rename folder: ${e}`);
    }
  };

  // Format shortcut array into Tauri-compatible string
  const formatHotkey = (keys: string[]): string => {
    return keys
      .map((k) => {
        if (k === 'Control') return 'Ctrl';
        if (k === 'Alt') return 'Alt';
        if (k === 'Shift') return 'Shift';
        if (k === 'Meta') return 'Cmd';
        if (k.startsWith('Key')) return k.slice(3);
        if (k.startsWith('Digit')) return k.slice(5);
        return k;
      })
      .join('+');
  };

  const handleSaveHotkey = async () => {
    if (savedShortcut.length > 0) {
      const newHotkey = formatHotkey(savedShortcut);
      await updateSetting('hotkey', newHotkey);
    }
    stopRecordingLib();
    setIsRecordingMode(false);
  };

  const handleCancelRecording = () => {
    stopRecordingLib();
    clearLastRecording();
    setIsRecordingMode(false);
  };

  const windowEffectValue =
    settings.mica_effect === 'clear'
      ? 'solid'
      : settings.mica_effect === 'mica_alt' || settings.mica_effect === 'auto'
        ? 'acrylic'
        : settings.mica_effect || 'solid';

  const customFolders = folders.filter((folder) => !folder.is_system);

  const tabs: { id: Tab; label: string; icon: ReactNode }[] = [
    { id: 'general', label: t('settings.general'), icon: <SettingsIcon size={17} /> },
    { id: 'privacy', label: t('settings.privacy'), icon: <ShieldCheck size={17} /> },
    { id: 'folders', label: t('settings.folders'), icon: <FolderIcon size={17} /> },
    { id: 'about', label: t('settings.about'), icon: <Info size={17} /> },
  ];

  const ghostButton =
    'inline-flex items-center gap-2 rounded-lg border border-border bg-accent/40 px-3 py-1.5 text-xs font-medium text-foreground transition-colors hover:bg-accent disabled:opacity-50';

  return (
    <>
      <ConfirmDialog
        isOpen={confirmDialog.isOpen}
        title={confirmDialog.title}
        message={confirmDialog.message}
        onConfirm={async () => {
          await confirmDialog.action();
          setConfirmDialog((prev) => ({ ...prev, isOpen: false }));
        }}
        onCancel={() => setConfirmDialog((prev) => ({ ...prev, isOpen: false }))}
      />
      <div className="flex h-full select-none flex-col bg-background text-foreground">
        {/* Title bar */}
        <div
          className="flex items-center justify-between border-b border-border px-4 py-3"
          onMouseDown={(e) => {
            if (e.button === 0) {
              getCurrentWindow().startDragging();
            }
          }}
        >
          <div className="flex items-center gap-2.5">
            <CubbyMark className="h-[18px] w-[18px]" />
            <span className="text-sm font-semibold">{t('settings.title')}</span>
          </div>
          <button
            onClick={onClose}
            className="icon-button"
            onMouseDown={(e) => e.stopPropagation()}
          >
            <X size={18} />
          </button>
        </div>

        <div className="flex flex-1 overflow-hidden">
          {/* Sidebar */}
          <div className="flex w-[188px] flex-shrink-0 flex-col border-r border-border bg-card/40 p-2.5">
            <div className="flex flex-col gap-0.5">
              {tabs.map((tab) => (
                <button
                  key={tab.id}
                  onClick={() => setActiveTab(tab.id)}
                  className={clsx(
                    'flex items-center gap-3 rounded-lg px-3 py-2 text-[13.5px] font-medium transition-colors',
                    activeTab === tab.id
                      ? 'bg-primary/10 text-foreground'
                      : 'text-muted-foreground hover:bg-accent/60 hover:text-foreground'
                  )}
                >
                  <span className={activeTab === tab.id ? 'text-primary' : ''}>{tab.icon}</span>
                  {tab.label}
                </button>
              ))}
            </div>
            <div className="mt-auto px-3 pt-3 text-[11px] leading-relaxed text-muted-foreground/70">
              {t('settings.sidebarNote')}
            </div>
          </div>

          {/* Content Area */}
          <div className="flex-1 overflow-y-auto px-7 py-6">
            <div className="mx-auto max-w-2xl">
              {/* --- GENERAL TAB --- */}
              {activeTab === 'general' && (
                <div className="space-y-7">
                  <PaneHeader
                    title={t('settings.general')}
                    subtitle={t('settings.generalSubtitle')}
                  />

                  <section>
                    <SectionLabel>{t('settings.appearance')}</SectionLabel>
                    <SettingCard>
                      <Row
                        title={t('settings.theme')}
                        control={
                          <Segmented
                            value={settings.theme}
                            onChange={handleThemeChange}
                            options={[
                              { value: 'dark', label: t('settings.themeDark') },
                              { value: 'system', label: t('settings.themeSystem') },
                              { value: 'light', label: t('settings.themeLight') },
                            ]}
                          />
                        }
                      />
                      <Row
                        title={t('settings.language')}
                        control={
                          <div className="w-40">
                            <Select
                              value={settings.language || 'en'}
                              onChange={handleLanguageChange}
                              options={[
                                { value: 'de', label: 'Deutsch' },
                                { value: 'en', label: 'English' },
                                { value: 'fr', label: 'Francais' },
                                { value: 'ja', label: '日本語' },
                                { value: 'zh', label: '中文' },
                              ]}
                            />
                          </div>
                        }
                      />
                      <Row
                        title={t('settings.windowEffect')}
                        desc={t('settings.windowEffectDesc')}
                        control={
                          <Segmented
                            value={windowEffectValue}
                            onChange={(val) => updateSetting('mica_effect', val)}
                            options={[
                              { value: 'solid', label: 'Solid' },
                              { value: 'mica', label: 'Mica' },
                              { value: 'acrylic', label: 'Acrylic' },
                            ]}
                          />
                        }
                      />
                      <Row
                        title={t('settings.density')}
                        desc={t('settings.densityDesc')}
                        control={
                          <Segmented
                            value={settings.density ?? 'comfortable'}
                            onChange={(val) => updateSetting('density', val)}
                            options={[
                              { value: 'comfortable', label: t('settings.densityComfortable') },
                              { value: 'compact', label: t('settings.densityCompact') },
                            ]}
                          />
                        }
                      />
                      <Row
                        title={t('settings.roundCorners')}
                        desc={t('settings.roundCornersDesc')}
                        control={
                          <Toggle
                            checked={settings.round_corners ?? false}
                            onChange={() =>
                              updateSetting('round_corners', !(settings.round_corners ?? false))
                            }
                            label={t('settings.roundCorners')}
                          />
                        }
                      />
                    </SettingCard>
                  </section>

                  <section>
                    <SectionLabel>{t('settings.windowBehavior')}</SectionLabel>
                    <SettingCard>
                      <Row
                        title={t('settings.startupWithWindows')}
                        desc={
                          settings.is_portable
                            ? t('settings.startupWithWindowsPortable')
                            : t('settings.startupWithWindowsDesc')
                        }
                        control={
                          <Toggle
                            checked={settings.startup_with_windows}
                            disabled={settings.is_portable}
                            onChange={() =>
                              updateSetting('startup_with_windows', !settings.startup_with_windows)
                            }
                            label={t('settings.startupWithWindows')}
                          />
                        }
                      />
                      <Row
                        title={t('settings.floatAboveTaskbar')}
                        desc={t('settings.floatAboveTaskbarDesc')}
                        control={
                          <Toggle
                            checked={settings.float_above_taskbar ?? true}
                            onChange={() =>
                              updateSetting(
                                'float_above_taskbar',
                                !(settings.float_above_taskbar ?? true)
                              )
                            }
                            label={t('settings.floatAboveTaskbar')}
                          />
                        }
                      />
                    </SettingCard>
                  </section>

                  <section>
                    <SectionLabel>{t('settings.shortcuts')}</SectionLabel>
                    <SettingCard>
                      <Row title={t('settings.openCubby')} desc={t('settings.hotkeyDesc')}>
                        {isRecordingMode ? (
                          <div className="space-y-2">
                            <div className="flex w-full items-center gap-2 rounded-lg border border-primary bg-input px-3 py-2 text-sm ring-2 ring-primary">
                              <span className="animate-pulse text-primary">
                                {shortcut.length > 0
                                  ? formatHotkey(shortcut)
                                  : savedShortcut.length > 0
                                    ? formatHotkey(savedShortcut)
                                    : t('settings.hotkeyRecording')}
                              </span>
                            </div>
                            <div className="flex gap-2">
                              <button
                                onClick={handleSaveHotkey}
                                disabled={savedShortcut.length === 0}
                                className="rounded bg-primary px-3 py-1 text-xs text-primary-foreground disabled:opacity-50"
                              >
                                {t('common.save')}
                              </button>
                              <button
                                onClick={handleCancelRecording}
                                className="rounded bg-muted px-3 py-1 text-xs text-muted-foreground"
                              >
                                {t('common.cancel')}
                              </button>
                            </div>
                          </div>
                        ) : (
                          <div className="flex items-center gap-2">
                            <span className="rounded-md border border-border bg-accent/50 px-2.5 py-1 font-mono text-xs font-medium">
                              {settings.hotkey}
                            </span>
                            <button onClick={handleStartRecording} className={ghostButton}>
                              {t('settings.changeHotkey')}
                            </button>
                          </div>
                        )}
                      </Row>
                      <Row
                        title={t('settings.replaceWinV')}
                        desc={t('settings.replaceWinVDesc')}
                        control={
                          <Toggle
                            checked={settings.replace_win_v}
                            onChange={() => updateSetting('replace_win_v', !settings.replace_win_v)}
                            label={t('settings.replaceWinV')}
                          />
                        }
                      />
                      <Row
                        title={t('settings.remotePasteMode')}
                        desc={t('settings.remotePasteModeDesc')}
                      >
                        <div className="grid grid-cols-2 gap-2">
                          <button
                            onClick={() => updateSetting('remote_paste_mode', 'copy_then_paste')}
                            className={clsx(
                              'rounded-lg border px-3 py-2 text-left text-xs transition-colors',
                              settings.remote_paste_mode === 'copy_then_paste'
                                ? 'border-primary/60 bg-primary/10 text-foreground'
                                : 'border-border bg-accent/30 text-muted-foreground hover:border-primary/40'
                            )}
                          >
                            <span className="block font-medium">
                              {t('settings.remotePasteCopy')}
                            </span>
                            <span className="mt-1 block leading-snug">
                              {t('settings.remotePasteCopyDesc')}
                            </span>
                          </button>
                          <button
                            onClick={() =>
                              updateSetting('remote_paste_mode', 'paste_as_keystrokes')
                            }
                            className={clsx(
                              'rounded-lg border px-3 py-2 text-left text-xs transition-colors',
                              settings.remote_paste_mode === 'paste_as_keystrokes'
                                ? 'border-primary/60 bg-primary/10 text-foreground'
                                : 'border-border bg-accent/30 text-muted-foreground hover:border-primary/40'
                            )}
                          >
                            <span className="block font-medium">
                              {t('settings.remotePasteKeystrokes')}
                            </span>
                            <span className="mt-1 block leading-snug">
                              {t('settings.remotePasteKeystrokesDesc')}
                            </span>
                          </button>
                        </div>
                        <p className="mt-3 text-xs text-muted-foreground">
                          {t('settings.remoteHotkeyHint')}
                        </p>
                      </Row>
                    </SettingCard>
                  </section>
                </div>
              )}

              {/* --- PRIVACY TAB --- */}
              {activeTab === 'privacy' && (
                <div className="space-y-7">
                  <PaneHeader
                    title={t('settings.privacyAndData')}
                    subtitle={t('settings.privacySubtitle')}
                  />

                  <section>
                    <SectionLabel>{t('settings.localHistory')}</SectionLabel>
                    <SettingCard>
                      <Row
                        title={
                          <span className="flex items-center gap-2">
                            {t('settings.clipboardHistory')}
                            <span className="inline-flex items-center gap-1.5 rounded-full border border-emerald-500/25 bg-emerald-500/10 px-2 py-0.5 text-[10px] font-semibold tracking-wide text-emerald-500">
                              <span className="h-1.5 w-1.5 rounded-full bg-emerald-500" />
                              AES-256-GCM
                            </span>
                          </span>
                        }
                        desc={t('settings.clipboardHistoryDesc')}
                      />
                      <Row
                        title={t('settings.skipSensitive')}
                        desc={t('settings.skipSensitiveDesc')}
                        control={
                          <Toggle
                            checked={settings.skip_sensitive ?? true}
                            onChange={() =>
                              updateSetting('skip_sensitive', !(settings.skip_sensitive ?? true))
                            }
                            label={t('settings.skipSensitive')}
                          />
                        }
                      />
                      <Row
                        title={t('settings.ignoreGhostClips')}
                        desc={t('settings.ignoreGhostClipsDesc')}
                        control={
                          <Toggle
                            checked={settings.ignore_ghost_clips}
                            onChange={() =>
                              updateSetting('ignore_ghost_clips', !settings.ignore_ghost_clips)
                            }
                            label={t('settings.ignoreGhostClips')}
                          />
                        }
                      />
                    </SettingCard>
                  </section>

                  <section>
                    <SectionLabel>{t('settings.historyRetention')}</SectionLabel>
                    <SettingCard>
                      <Row
                        title={t('settings.keepHistoryFor')}
                        control={
                          <div className="w-40">
                            <Select
                              value={String(settings.auto_delete_days ?? 30)}
                              onChange={handleRetentionChange}
                              options={[
                                { value: '7', label: t('settings.retention7') },
                                { value: '30', label: t('settings.retention30') },
                                { value: '90', label: t('settings.retention90') },
                                { value: '365', label: t('settings.retention365') },
                                { value: '0', label: t('settings.retentionForever') },
                              ]}
                            />
                          </div>
                        }
                      />
                      {settings.auto_delete_days === 0 && (
                        <div className="flex gap-3 px-4 py-3">
                          <AlertTriangle
                            size={16}
                            className="mt-0.5 flex-shrink-0 text-amber-500"
                          />
                          <p className="text-xs leading-relaxed text-muted-foreground">
                            {t('settings.retentionForeverWarning')}
                          </p>
                        </div>
                      )}
                      <Row
                        title={t('settings.storageUsed')}
                        control={
                          <span className="text-xs text-muted-foreground">
                            {storageUsage
                              ? `${t('settings.folderItemCount', {
                                  count: storageUsage.items,
                                })} · ${formatBytes(storageUsage.bytes)}`
                              : '…'}
                          </span>
                        }
                      />
                    </SettingCard>
                    <p className="ml-1 mt-2 text-xs text-muted-foreground">
                      {t('settings.pinnedAlwaysKept')}
                    </p>
                  </section>

                  <section>
                    <SectionLabel>{t('settings.ignoredApps')}</SectionLabel>
                    <p className="-mt-1 mb-2 ml-1 text-xs text-muted-foreground">
                      {t('settings.ignoredAppsDesc')}
                    </p>
                    <SettingCard>
                      <div className="p-2">
                        {ignoredApps.length === 0 ? (
                          <p className="px-2 py-3 text-center text-xs text-muted-foreground">
                            {t('settings.noIgnoredApps')}
                          </p>
                        ) : (
                          <div className="space-y-0.5">
                            {ignoredApps.map((app) => (
                              <div
                                key={app}
                                className="group flex items-center gap-3 rounded-lg px-2.5 py-2 hover:bg-accent/50"
                              >
                                <span className="flex h-7 w-7 flex-shrink-0 items-center justify-center rounded-md border border-border bg-accent/40 text-muted-foreground">
                                  <Lock size={14} />
                                </span>
                                <span className="flex-1 font-mono text-xs">{app}</span>
                                <button
                                  onClick={() => handleRemoveIgnoredApp(app)}
                                  className="rounded-md p-1 text-muted-foreground opacity-0 transition hover:bg-destructive/10 hover:text-destructive group-hover:opacity-100"
                                  aria-label={`Remove ${app}`}
                                >
                                  <X size={14} />
                                </button>
                              </div>
                            ))}
                          </div>
                        )}
                      </div>
                      <div className="flex gap-2 px-3 py-3">
                        <input
                          type="text"
                          value={newIgnoredApp}
                          onChange={(e) => setNewIgnoredApp(e.target.value)}
                          placeholder="notepad.exe"
                          className="flex-1 rounded-lg border border-border bg-input px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-ring"
                          onKeyDown={(e) => e.key === 'Enter' && handleAddIgnoredApp()}
                        />
                        <button
                          onClick={handleBrowseFile}
                          className={ghostButton}
                          title="Browse executable"
                        >
                          <FolderOpen size={14} />
                        </button>
                        <button
                          onClick={handleAddIgnoredApp}
                          disabled={!newIgnoredApp.trim()}
                          className={ghostButton}
                        >
                          <Plus size={14} />
                          {t('settings.add')}
                        </button>
                      </div>
                    </SettingCard>
                  </section>

                  <section>
                    <SectionLabel>{t('settings.screenshotText')}</SectionLabel>
                    <SettingCard>
                      <Row>
                        <div className="flex items-start justify-between gap-4">
                          <div className="min-w-0">
                            <div className="text-sm font-medium">{ocrStatusLabel}</div>
                            <p className="mt-1 text-xs leading-snug text-muted-foreground">
                              {t('settings.ocrIndexDesc')}
                            </p>
                            {ocrStatus && (
                              <p className="mt-2 text-xs text-muted-foreground">
                                {t('settings.ocrProgress', {
                                  completed: ocrStatus.completed,
                                  remaining: ocrRemaining,
                                  failed: ocrFailures,
                                })}
                              </p>
                            )}
                            {!!ocrStatus?.unavailable && (
                              <p className="mt-2 text-xs text-amber-600 dark:text-amber-400">
                                {t('settings.ocrUnavailableDesc')}
                              </p>
                            )}
                          </div>
                          <button
                            onClick={handleOcrPauseToggle}
                            disabled={!ocrStatus || ocrActionBusy}
                            className={clsx(ghostButton, 'flex-shrink-0')}
                          >
                            {ocrStatus?.paused ? <Play size={14} /> : <Pause size={14} />}
                            {ocrStatus?.paused ? t('common.resume') : t('common.pause')}
                          </button>
                        </div>
                        {!!ocrStatus && ocrFailures > 0 && (
                          <button
                            onClick={handleRetryOcr}
                            disabled={ocrActionBusy}
                            className={clsx(ghostButton, 'mt-3')}
                          >
                            <RefreshCw size={14} />
                            {t('settings.ocrRetry')}
                          </button>
                        )}
                      </Row>
                    </SettingCard>
                  </section>

                  <section>
                    <SectionLabel>{t('settings.yourData')}</SectionLabel>
                    <SettingCard>
                      <Row
                        title={t('settings.dittoImport')}
                        desc={t('settings.dittoImportDesc')}
                        control={
                          <button
                            onClick={handleImportFromDitto}
                            disabled={dittoBusy}
                            className={ghostButton}
                          >
                            {dittoBusy
                              ? t('settings.dittoImporting')
                              : t('settings.dittoImportButton')}
                          </button>
                        }
                      />
                      <Row
                        title={t('settings.removeDuplicates')}
                        desc={t('settings.removeDuplicatesDesc')}
                        control={
                          <button onClick={handleRemoveDuplicates} className={ghostButton}>
                            {t('settings.removeDuplicatesButton')}
                          </button>
                        }
                      />
                      <Row
                        title={t('settings.clearHistory')}
                        desc={t('settings.clearHistoryDesc')}
                        control={
                          <button
                            onClick={confirmClearHistory}
                            className="inline-flex items-center gap-2 rounded-lg border border-destructive/25 bg-destructive/10 px-3 py-1.5 text-xs font-medium text-destructive transition-colors hover:bg-destructive/20"
                          >
                            <Trash2 size={14} />
                            {t('settings.clearHistory')}
                          </button>
                        }
                      />
                    </SettingCard>
                  </section>

                  <div className="flex gap-3 rounded-xl border border-primary/20 bg-primary/[0.06] p-3.5">
                    <ShieldCheck size={16} className="mt-0.5 flex-shrink-0 text-primary" />
                    <p className="text-xs leading-relaxed text-muted-foreground">
                      <span className="font-semibold text-foreground">
                        {t('settings.privacyNoteTitle')}
                      </span>{' '}
                      {t('settings.privacyNote')}
                    </p>
                  </div>
                </div>
              )}

              {/* --- FOLDERS TAB --- */}
              {activeTab === 'folders' && (
                <div className="space-y-7">
                  <PaneHeader
                    title={t('settings.folders')}
                    subtitle={t('settings.foldersSubtitle')}
                  />
                  <section>
                    <SectionLabel>{t('settings.manageFolders')}</SectionLabel>
                    <SettingCard>
                      <div className="p-2">
                        {customFolders.length === 0 ? (
                          <p className="px-2 py-3 text-center text-xs text-muted-foreground">
                            {t('settings.noFolders')}
                          </p>
                        ) : (
                          <div className="space-y-0.5">
                            {customFolders.map((folder) => (
                              <div
                                key={folder.id}
                                className="group flex items-center gap-3 rounded-lg px-2.5 py-2 hover:bg-accent/50"
                              >
                                {editingFolderId === folder.id ? (
                                  <div className="flex flex-1 items-center gap-2">
                                    <input
                                      type="text"
                                      value={renameValue}
                                      onChange={(e) => setRenameValue(e.target.value)}
                                      className="flex-1 rounded-md border border-input bg-background px-2 py-1 text-sm"
                                      autoFocus
                                      onKeyDown={(e) => {
                                        if (e.key === 'Enter') saveRenameFolder();
                                        if (e.key === 'Escape') setEditingFolderId(null);
                                      }}
                                    />
                                    <button
                                      onClick={saveRenameFolder}
                                      className="text-xs text-primary hover:underline"
                                    >
                                      {t('common.save')}
                                    </button>
                                    <button
                                      onClick={() => setEditingFolderId(null)}
                                      className="text-xs text-muted-foreground hover:underline"
                                    >
                                      {t('common.cancel')}
                                    </button>
                                  </div>
                                ) : (
                                  <>
                                    <span className="flex h-7 w-7 flex-shrink-0 items-center justify-center rounded-md border border-border bg-accent/40 text-primary">
                                      <FolderIcon size={14} />
                                    </span>
                                    <span className="flex-1 text-sm font-medium">
                                      {folder.name}
                                      <span className="ml-2 text-xs font-normal text-muted-foreground">
                                        {t('settings.folderItemCount', {
                                          count: folder.item_count,
                                        })}
                                      </span>
                                    </span>
                                    <button
                                      onClick={() => startRenameFolder(folder)}
                                      className="rounded-md p-1 text-muted-foreground opacity-0 transition hover:bg-accent hover:text-foreground group-hover:opacity-100"
                                      title="Rename"
                                    >
                                      <MoreHorizontal size={14} />
                                    </button>
                                    <button
                                      onClick={() => handleDeleteFolder(folder.id)}
                                      className="rounded-md p-1 text-muted-foreground opacity-0 transition hover:bg-destructive/10 hover:text-destructive group-hover:opacity-100"
                                      title="Delete"
                                    >
                                      <Trash2 size={14} />
                                    </button>
                                  </>
                                )}
                              </div>
                            ))}
                          </div>
                        )}
                      </div>
                      <div className="flex gap-2 px-3 py-3">
                        <input
                          type="text"
                          value={newFolderName}
                          onChange={(e) => setNewFolderName(e.target.value)}
                          placeholder={t('settings.newFolderPlaceholder')}
                          className="flex-1 rounded-lg border border-border bg-input px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-ring"
                          onKeyDown={(e) => e.key === 'Enter' && handleCreateFolder()}
                        />
                        <button
                          onClick={handleCreateFolder}
                          disabled={!newFolderName.trim()}
                          className={ghostButton}
                        >
                          <Plus size={14} />
                          {t('settings.add')}
                        </button>
                      </div>
                    </SettingCard>
                  </section>
                </div>
              )}

              {/* --- ABOUT TAB --- */}
              {activeTab === 'about' && (
                <div className="space-y-7">
                  <PaneHeader title={t('settings.about')} subtitle={t('settings.aboutSubtitle')} />
                  <section>
                    <SettingCard>
                      <div className="flex items-center gap-4 px-4 py-4">
                        <CubbyMark className="h-11 w-11 flex-shrink-0" />
                        <div className="min-w-0 flex-1">
                          <div className="text-base font-semibold">Cubby</div>
                          <div className="text-xs text-muted-foreground">
                            {t('settings.versionLabel', { version: appVersion || '…' })}
                          </div>
                        </div>
                        <button
                          onClick={handleCheckUpdates}
                          disabled={checkingUpdate}
                          className={ghostButton}
                        >
                          <RefreshCw size={14} className={checkingUpdate ? 'animate-spin' : ''} />
                          {t('settings.checkUpdates')}
                        </button>
                      </div>
                      <button
                        onClick={() => openUrl(GITHUB_URL).catch(console.error)}
                        className="flex w-full items-center gap-3 px-4 py-3 text-sm transition-colors hover:bg-accent/40"
                      >
                        <Github size={16} className="text-muted-foreground" />
                        <span className="flex-1 text-left">{t('settings.sourceCode')}</span>
                        <ExternalLink size={14} className="text-muted-foreground" />
                      </button>
                      <button
                        onClick={() => openUrl(WEBSITE_URL).catch(console.error)}
                        className="flex w-full items-center gap-3 px-4 py-3 text-sm transition-colors hover:bg-accent/40"
                      >
                        <Globe size={16} className="text-muted-foreground" />
                        <span className="flex-1 text-left">cubbyclipboard.com</span>
                        <ExternalLink size={14} className="text-muted-foreground" />
                      </button>
                      <button
                        onClick={() => openUrl(PRIVACY_URL).catch(console.error)}
                        className="flex w-full items-center gap-3 px-4 py-3 text-sm transition-colors hover:bg-accent/40"
                      >
                        <ShieldCheck size={16} className="text-muted-foreground" />
                        <span className="flex-1 text-left">{t('settings.privacyPolicy')}</span>
                        <ExternalLink size={14} className="text-muted-foreground" />
                      </button>
                    </SettingCard>
                    <div className="mt-3 flex gap-3 rounded-xl border border-border bg-card/60 p-3.5">
                      <Info size={16} className="mt-0.5 flex-shrink-0 text-muted-foreground" />
                      <p className="text-xs leading-relaxed text-muted-foreground">
                        <span className="font-semibold text-foreground">
                          {t('settings.openSourceTitle')}
                        </span>{' '}
                        {t('settings.openSourceNote')}
                      </p>
                    </div>
                  </section>
                </div>
              )}
            </div>
          </div>
        </div>

        {/* Debug Tools — dev build only */}
        {import.meta.env.DEV && (
          <div className="border-t border-border px-4 py-3">
            <p className="mb-2 text-xs font-medium text-muted-foreground">Debug</p>
            <div className="flex gap-2">
              <button
                onClick={() => emit('load-demo-data')}
                className="flex items-center gap-2 rounded-md border border-dashed border-border px-3 py-1.5 text-xs text-muted-foreground transition-colors hover:border-primary hover:text-primary"
              >
                <FlaskConical size={12} />
                Load 20 demo clips
              </button>
              <button
                onClick={() => emit('restore-actual-data')}
                className="flex items-center gap-2 rounded-md border border-dashed border-border px-3 py-1.5 text-xs text-muted-foreground transition-colors hover:border-destructive hover:text-destructive"
              >
                Restore actual data
              </button>
            </div>
          </div>
        )}
      </div>
    </>
  );
}
