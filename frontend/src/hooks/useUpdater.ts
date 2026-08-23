import { useEffect } from 'react';
import { check, type Update } from '@tauri-apps/plugin-updater';
import { relaunch } from '@tauri-apps/plugin-process';
import { toast } from 'sonner';
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
  useEffect(() => {
    if (!enabled) return;

    return startUpdateChecks({
      check,
      announce: (update) => {
        // Safe: the scheduler only announces an update it got from check().
        const available = update as Update;
        toast(`Cubby ${available.version} is available.`, {
          duration: Infinity,
          action: {
            label: 'Update now',
            onClick: () => void installUpdate(available),
          },
        });
      },
      onError: (error) => console.error('Update check failed:', error),
    });
  }, [enabled]);
}

async function installUpdate(update: Update) {
  const toastId = toast.loading('Downloading update…');
  try {
    await update.downloadAndInstall();
    toast.success('Update ready — restarting Cubby…', { id: toastId });
    await relaunch();
  } catch (error) {
    console.error('Update install failed:', error);
    toast.error('Update failed. Please try again later.', { id: toastId });
  }
}
