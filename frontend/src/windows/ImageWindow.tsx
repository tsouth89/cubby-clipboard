import { useCallback, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { Clock, Copy, Image as ImageIcon, Type, X } from 'lucide-react';
import { Toaster, toast } from 'sonner';
import { ClipDetails } from '../components/ClipPreview';
import { ImageTextViewer } from '../components/ImageTextViewer';
import { imageSrcFromContent } from '../utils/clipDisplay';
import { useSystemAccent } from '../hooks/useSystemAccent';
import { useTheme } from '../hooks/useTheme';
import { Settings } from '../types';

/**
 * A screenshot on its own, with room to breathe: opens at actual size so the
 * captured text is legible, and the whole window is the selection surface.
 *
 * The History window's preview pane is a few hundred pixels wide, which is
 * where this started — at fit-to-pane a desktop screenshot's text is far too
 * small to read, let alone select word by word.
 */
export function ImageWindow() {
  const [details, setDetails] = useState<ClipDetails | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [settings, setSettings] = useState<Settings | null>(null);

  const effectiveTheme = useTheme(settings?.theme ?? 'system');
  // Each Tauri window is its own document and must apply the accent itself.
  useSystemAccent();

  const clipId = new URLSearchParams(window.location.search).get('clip');

  useEffect(() => {
    invoke<Settings>('get_settings').then(setSettings).catch(console.error);
  }, []);

  useEffect(() => {
    if (!clipId) {
      setError('No clip was requested.');
      return;
    }
    let active = true;
    invoke<ClipDetails>('get_clip_details', { id: clipId })
      .then((loaded) => {
        if (active) setDetails(loaded);
      })
      .catch((loadError) => {
        console.error('Failed to load the image:', loadError);
        if (active) setError(String(loadError));
      });
    return () => {
      active = false;
    };
  }, [clipId]);

  const handleClose = useCallback(() => {
    getCurrentWindow()
      .close()
      .catch((closeError) => console.error('Failed to close the image window:', closeError));
  }, []);

  const handleCopySelection = useCallback(async (text: string) => {
    try {
      await invoke('copy_selected_text', { text });
      toast.success('Copied selection');
    } catch (copyError) {
      console.error('Failed to copy the selection:', copyError);
      toast.error('Failed to copy');
    }
  }, []);

  const handleCopyImage = useCallback(async () => {
    if (!clipId) return;
    try {
      await invoke('copy_clip', { id: clipId, plainText: false });
      toast.success('Copied');
    } catch (copyError) {
      console.error('Failed to copy the image:', copyError);
      toast.error('Failed to copy');
    }
  }, [clipId]);

  const handleCopyAllText = useCallback(async () => {
    if (!clipId) return;
    try {
      await invoke('copy_ocr_text', { id: clipId });
      toast.success('Copied');
    } catch (copyError) {
      console.error('Failed to copy the recognized text:', copyError);
      toast.error('Failed to copy');
    }
  }, [clipId]);

  // Escape closes, unless the viewer claims it first to clear a live selection.
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') handleClose();
    };
    document.addEventListener('keydown', onKeyDown);
    return () => document.removeEventListener('keydown', onKeyDown);
  }, [handleClose]);

  const imageSrc = details ? imageSrcFromContent(details.content) : null;
  const actionClass =
    'inline-flex items-center gap-1.5 rounded-md border border-white/[0.09] bg-white/[0.05] px-2.5 py-1.5 text-[11px] font-medium text-foreground transition-colors hover:bg-white/[0.1]';

  return (
    <div className="flex h-screen select-none flex-col bg-background text-foreground">
      <header
        className="flex shrink-0 items-center justify-between border-b border-border px-4 py-2.5"
        onMouseDown={(event) => {
          if (event.button === 0) getCurrentWindow().startDragging();
        }}
      >
        <div className="flex items-center gap-2.5">
          <ImageIcon size={16} />
          <span className="text-sm font-semibold">Image</span>
          {details?.image_expired && (
            <span className="flex items-center gap-1 text-[11px] text-muted-foreground">
              <Clock size={11} />
              Full image expired; showing the kept thumbnail
            </span>
          )}
        </div>
        <div className="flex items-center gap-1.5" onMouseDown={(event) => event.stopPropagation()}>
          <button type="button" className={actionClass} onClick={handleCopyImage}>
            <Copy size={13} />
            Copy image
          </button>
          {details?.ocr_text && (
            <button type="button" className={actionClass} onClick={handleCopyAllText}>
              <Type size={13} />
              Copy all text
            </button>
          )}
          <button
            type="button"
            onClick={handleClose}
            className="icon-button ml-1"
            aria-label="Close image"
          >
            <X size={18} />
          </button>
        </div>
      </header>

      <main className="min-h-0 flex-1 px-3 pb-2 pt-3">
        {imageSrc ? (
          <ImageTextViewer
            src={imageSrc}
            words={details?.ocr_layout?.words ?? []}
            dimmed={details?.image_expired}
            initialZoom="actual"
            onCopySelection={handleCopySelection}
          />
        ) : (
          <div className="flex h-full items-center justify-center">
            <p className="text-xs text-muted-foreground">
              {error ? 'Could not load this image.' : 'Loading…'}
            </p>
          </div>
        )}
      </main>
      <Toaster richColors position="bottom-center" theme={effectiveTheme} />
    </div>
  );
}
