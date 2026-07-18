import { useEffect, useRef } from 'react';
import { check, type Update } from '@tauri-apps/plugin-updater';
import { relaunch } from '@tauri-apps/plugin-process';
import { toast } from 'sonner';
import { useTranslation } from 'react-i18next';
import type { TFunction } from 'i18next';

/**
 * Checks for an app update once on startup. If one is available it shows a
 * friendly, non-blocking prompt and lets the user choose when to install.
 * Any failure (offline, GitHub unreachable, dev build) is swallowed so it
 * never interrupts normal use.
 */
export function useUpdater() {
  const { t } = useTranslation();
  const checkedRef = useRef(false);

  useEffect(() => {
    if (checkedRef.current) return;
    checkedRef.current = true;

    void (async () => {
      let update: Update | null = null;
      try {
        update = await check();
      } catch (error) {
        console.error('Update check failed:', error);
        return;
      }
      if (!update?.available) return;

      const available = update;
      toast(t('updater.available', { version: available.version }), {
        duration: Infinity,
        action: {
          label: t('updater.install'),
          onClick: () => void installUpdate(available, t),
        },
      });
    })();
  }, [t]);
}

async function installUpdate(update: Update, t: TFunction) {
  const toastId = toast.loading(t('updater.installing'));
  try {
    await update.downloadAndInstall();
    toast.success(t('updater.restarting'), { id: toastId });
    await relaunch();
  } catch (error) {
    console.error('Update install failed:', error);
    toast.error(t('updater.failed'), { id: toastId });
  }
}
