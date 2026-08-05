import { useEffect } from 'react';
import { check, type Update } from '@tauri-apps/plugin-updater';
import { relaunch } from '@tauri-apps/plugin-process';
import { toast } from 'sonner';
import { useTranslation } from 'react-i18next';
import type { TFunction } from 'i18next';
import { startUpdateChecks } from './updateSchedule';

/**
 * Checks for an app update on startup and every 30 minutes while Cubby is
 * running. If one is available it shows a friendly, non-blocking prompt and
 * lets the user choose when to install.
 * Any failure (offline, GitHub unreachable, dev build) is swallowed so it
 * never interrupts normal use.
 *
 * The scheduling itself lives in `updateSchedule.ts`, free of React and Tauri
 * so it can be regression-tested; this hook only wires it to the real plugin
 * and the toast.
 */
export function useUpdater(enabled: boolean) {
  const { t } = useTranslation();

  useEffect(() => {
    if (!enabled) return;

    return startUpdateChecks({
      check,
      announce: (update) => {
        // Safe: the scheduler only announces an update it got from check().
        const available = update as Update;
        toast(t('updater.available', { version: available.version }), {
          duration: Infinity,
          action: {
            label: t('updater.install'),
            onClick: () => void installUpdate(available, t),
          },
        });
      },
      onError: (error) => console.error('Update check failed:', error),
    });
  }, [enabled, t]);
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
