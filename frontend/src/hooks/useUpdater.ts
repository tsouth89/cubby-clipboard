import { useEffect, useRef } from 'react';
import { check, type Update } from '@tauri-apps/plugin-updater';
import { relaunch } from '@tauri-apps/plugin-process';
import { toast } from 'sonner';
import { useTranslation } from 'react-i18next';
import type { TFunction } from 'i18next';

const UPDATE_CHECK_INTERVAL_MS = 30 * 60 * 1000;

/**
 * Checks for an app update on startup and every 30 minutes while Cubby is
 * running. If one is available it shows a friendly, non-blocking prompt and
 * lets the user choose when to install.
 * Any failure (offline, GitHub unreachable, dev build) is swallowed so it
 * never interrupts normal use.
 */
export function useUpdater() {
  const { t } = useTranslation();
  const checkInFlightRef = useRef(false);
  const notifiedVersionRef = useRef<string | null>(null);

  useEffect(() => {
    let active = true;

    const checkForUpdate = async () => {
      if (checkInFlightRef.current) return;
      checkInFlightRef.current = true;

      let update: Update | null = null;
      try {
        update = await check();
      } catch (error) {
        console.error('Update check failed:', error);
      } finally {
        checkInFlightRef.current = false;
      }

      if (!active || !update?.available || notifiedVersionRef.current === update.version) {
        return;
      }

      notifiedVersionRef.current = update.version;

      const available = update;
      toast(t('updater.available', { version: available.version }), {
        duration: Infinity,
        action: {
          label: t('updater.install'),
          onClick: () => void installUpdate(available, t),
        },
      });
    };

    void checkForUpdate();
    const intervalId = window.setInterval(() => void checkForUpdate(), UPDATE_CHECK_INTERVAL_MS);

    return () => {
      active = false;
      window.clearInterval(intervalId);
    };
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
