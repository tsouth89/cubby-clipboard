import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { Settings } from '../types';
import { SettingsPanel } from '../components/SettingsPanel';
import { useTheme } from '../hooks/useTheme';
import { useLanguage } from '../hooks/useLanguage';

import { Toaster } from 'sonner';

export function SettingsWindow() {
  const [settings, setSettings] = useState<Settings | null>(null);

  const effectiveTheme = useTheme(settings?.theme || 'system');
  useLanguage(settings?.language);

  useEffect(() => {
    invoke<Settings>('get_settings').then(setSettings).catch(console.error);
  }, []);

  const handleClose = async () => {
    const win = getCurrentWindow();
    try {
      await win.close();
    } catch (e) {
      console.error('Failed to close settings window:', e);
    }
  };

  if (!settings) {
    return <div className="flex h-screen items-center justify-center text-white">Loading...</div>;
  }

  return (
    <div className="h-screen">
      <div className="h-full overflow-hidden bg-background text-foreground">
        <SettingsPanel settings={settings} onClose={handleClose} />
        <Toaster richColors position="bottom-center" theme={effectiveTheme} />
      </div>
    </div>
  );
}
