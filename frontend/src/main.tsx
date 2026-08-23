import ReactDOM from 'react-dom/client';
import { lazy, Suspense } from 'react';
import { attachConsole } from '@tauri-apps/plugin-log';
import './i18n/config'; // Initialize i18n
import './index.css';

attachConsole()
  .then(() => console.log('[WinPaste] Tauri console attached successfully'))
  .catch((err) => console.error('[WinPaste] Failed to attach Tauri console:', err));
console.log('[WinPaste] Frontend loaded - if you see this, DevTools is working!');

const urlParams = new URLSearchParams(window.location.search);
const windowType = urlParams.get('window');
const WindowRoot = lazy(async () => {
  if (windowType === 'settings') {
    const { SettingsWindow } = await import('./windows/SettingsWindow');
    return { default: SettingsWindow };
  }
  if (windowType === 'history') {
    const { HistoryWindow } = await import('./windows/HistoryWindow');
    return { default: HistoryWindow };
  }
  if (windowType === 'image') {
    const { ImageWindow } = await import('./windows/ImageWindow');
    return { default: ImageWindow };
  }
  return import('./App');
});

const initialTheme = window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light';
document.documentElement.classList.add(initialTheme);

ReactDOM.createRoot(document.getElementById('root')!).render(
  <Suspense
    fallback={
      <div className="h-full w-full bg-background" role="status" aria-label="Loading Cubby" />
    }
  >
    <WindowRoot />
  </Suspense>
);
