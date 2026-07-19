import { useSyncExternalStore } from 'react';

const listeners = new Set<() => void>();
let intervalId: ReturnType<typeof setInterval> | null = null;
let version = 0;

const REFRESH_MS = 15000;

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  if (intervalId === null) {
    intervalId = setInterval(() => {
      version += 1;
      listeners.forEach((l) => l());
    }, REFRESH_MS);
  }
  return () => {
    listeners.delete(listener);
    if (listeners.size === 0 && intervalId !== null) {
      clearInterval(intervalId);
      intervalId = null;
    }
  };
}

function getSnapshot(): number {
  return version;
}

/**
 * Re-renders subscribers every 15 seconds off a single shared interval, so
 * relative timestamps ("2 minutes ago") keep advancing while the flyout stays
 * open instead of freezing at whatever they were when the list first rendered.
 */
export function useTimeTick(): number {
  return useSyncExternalStore(subscribe, getSnapshot);
}
