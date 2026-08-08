import { useEffect, useMemo, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { clsx } from 'clsx';
import { format } from 'date-fns';
import { Clock, Copy, ExternalLink, Pin, Trash2, Type } from 'lucide-react';
import { ClipboardItem } from '../types';
import { ImageTextViewer } from './ImageTextViewer';
import { OcrLayout } from '../utils/ocrSelection';
import {
  contentKind,
  formatBytes,
  imageLabel,
  imageSrcFromContent,
  parseImageMetadata,
  sourceLabel,
} from '../utils/clipDisplay';

/** Payload of the `get_clip_details` command. */
export interface ClipDetails {
  content: string;
  ocr_text: string | null;
  image_expired: boolean;
  ocr_layout: OcrLayout | null;
}

interface ClipPreviewProps {
  clip: ClipboardItem | null;
  onCopy: (clipId: string, plainText?: boolean) => void;
  onCopyOcrText: (clipId: string) => void;
  onCopySelection: (text: string) => void;
  /** Pops the image out into its own full-size window. */
  onOpenImage: (clipId: string) => void;
  onTogglePin: (clipId: string) => void;
  onDelete: (clipId: string) => void;
}

function MetaRow({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex items-baseline gap-2 text-[11px]">
      <span className="w-16 shrink-0 text-muted-foreground">{label}</span>
      <span className="min-w-0 break-words text-foreground/85">{value}</span>
    </div>
  );
}

export function ClipPreview({
  clip,
  onCopy,
  onCopyOcrText,
  onCopySelection,
  onOpenImage,
  onTogglePin,
  onDelete,
}: ClipPreviewProps) {
  const [details, setDetails] = useState<ClipDetails | null>(null);
  const [detailsError, setDetailsError] = useState<string | null>(null);
  const clipId = clip?.id ?? null;

  // The list row carries only a preview (and a thumbnail for images). Pull the
  // full payload when a clip is actually selected, and drop a late response for
  // a clip that is no longer selected so a slow load can't overwrite a newer one.
  // Refetch when OCR lands for the clip already on screen: the id has not
  // changed, so without this the pane keeps showing the pre-OCR payload — no
  // recognized text and no selectable words — until you navigate away and back.
  const ocrReady = clip?.has_ocr_text ?? false;

  // Clear only when the clip itself changes. An OCR refetch is for the text and
  // word boxes; blanking the pane would drop the already-loaded image back to
  // "Loading…" and re-transfer the whole blob for no visual gain.
  const previousClipId = useRef<string | null>(null);

  useEffect(() => {
    if (previousClipId.current !== clipId) {
      previousClipId.current = clipId;
      setDetails(null);
      setDetailsError(null);
    }
    if (!clipId) return;

    let active = true;
    invoke<ClipDetails>('get_clip_details', { id: clipId })
      .then((loaded) => {
        if (active) setDetails(loaded);
      })
      .catch((error) => {
        console.error('Failed to load clip details:', error);
        if (active) setDetailsError(String(error));
      });

    return () => {
      active = false;
    };
  }, [clipId, ocrReady]);

  const isImage = clip?.clip_type === 'image';
  const imageMetadata = useMemo(() => parseImageMetadata(clip?.metadata), [clip?.metadata]);
  // Wait for the full payload rather than showing the row's thumbnail first.
  // Swapping the image under the viewer means fit and 1:1 are computed against
  // the thumbnail's dimensions and then recomputed against the real ones, which
  // visibly jumps the zoom. The thumbnail is only used if the full load failed,
  // where it is the best thing left to show.
  const imageSrc = useMemo(() => {
    if (!isImage) return null;
    if (details) return imageSrcFromContent(details.content);
    return detailsError ? imageSrcFromContent(clip?.content) : null;
  }, [isImage, details, detailsError, clip?.content]);

  if (!clip) {
    return (
      <div className="flex h-full items-center justify-center px-10 text-center">
        <p className="text-xs text-muted-foreground">Select a clip to preview it here.</p>
      </div>
    );
  }

  const label = sourceLabel(clip.source_app, clip.clip_type);
  const text = isImage ? '' : (details?.content ?? clip.content ?? clip.preview);
  const kind = isImage ? imageLabel(label) : contentKind(text, clip.clip_type);
  const captured = (() => {
    const parsed = new Date(clip.created_at);
    return Number.isNaN(parsed.getTime()) ? null : format(parsed, 'PPpp');
  })();
  const dimensions =
    imageMetadata.width && imageMetadata.height
      ? `${imageMetadata.width}×${imageMetadata.height}`
      : null;
  const size = isImage ? formatBytes(imageMetadata.size_bytes) : `${text.length} characters`;
  const expired = details?.image_expired ?? clip.image_expired ?? false;
  const actionClass =
    'inline-flex items-center gap-1.5 rounded-md border border-white/[0.09] bg-white/[0.05] px-2.5 py-1.5 text-[11px] font-medium text-foreground transition-colors hover:bg-white/[0.1] disabled:opacity-40';

  return (
    <div data-el="clip-preview" className="flex h-full min-h-0 flex-col">
      {isImage ? (
        <div className="flex min-h-0 flex-1 flex-col px-3 pt-3">
          {imageSrc ? (
            <ImageTextViewer
              src={imageSrc}
              words={details?.ocr_layout?.words ?? []}
              dimmed={expired}
              onCopySelection={onCopySelection}
              actions={
                <button
                  type="button"
                  onClick={() => onOpenImage(clip.id)}
                  className="ml-2 inline-flex items-center gap-1 rounded-md border border-white/[0.09] bg-white/[0.05] px-2 py-1 text-[11px] font-medium text-foreground transition-colors hover:bg-white/[0.1]"
                  title="Open this image in its own window at full size"
                >
                  <ExternalLink size={11} />
                  Open full size
                </button>
              }
            />
          ) : (
            <p className="text-xs text-muted-foreground">
              {detailsError ? 'No image data.' : 'Loading image…'}
            </p>
          )}
          {expired && (
            <p className="flex shrink-0 items-center gap-1.5 pt-1 text-[11px] text-muted-foreground">
              <Clock size={11} className="shrink-0" />
              The full image expired by retention. Its recognized text was kept.
            </p>
          )}
          {details?.ocr_text && (
            <details className="mt-2 shrink-0 border-t border-white/[0.07] pt-2">
              <summary className="cursor-pointer text-[11px] text-muted-foreground">
                All recognized text
              </summary>
              <pre className="mt-1.5 max-h-28 overflow-y-auto whitespace-pre-wrap break-words text-[12px] leading-[18px] text-foreground/80">
                {details.ocr_text}
              </pre>
            </details>
          )}
          {detailsError && (
            <p className="shrink-0 pt-2 text-[11px] text-destructive">
              Could not load the full image. Showing the list thumbnail instead.
            </p>
          )}
        </div>
      ) : (
        <div className="min-h-0 flex-1 overflow-y-auto p-4">
          <pre
            className={clsx(
              'whitespace-pre-wrap break-words text-[12.5px] leading-[19px] text-foreground/95',
              kind === 'Code' && 'font-mono text-[12px]'
            )}
          >
            {text}
          </pre>
          {detailsError && (
            <p className="mt-3 text-[11px] text-destructive">
              Could not load the full clip. Showing the list preview instead.
            </p>
          )}
        </div>
      )}

      <div className="shrink-0 space-y-1 border-t border-white/[0.07] px-4 py-3">
        <MetaRow label="Source" value={label} />
        <MetaRow label="Kind" value={kind} />
        {captured && <MetaRow label="Captured" value={captured} />}
        {dimensions && <MetaRow label="Size" value={dimensions} />}
        {size && <MetaRow label={dimensions ? 'On disk' : 'Length'} value={size} />}
      </div>

      <div className="flex shrink-0 flex-wrap items-center gap-1.5 border-t border-white/[0.07] px-4 py-3">
        <button
          type="button"
          className={actionClass}
          onClick={() => onCopy(clip.id)}
          disabled={isImage && expired}
          title={isImage && expired ? 'The full image expired by retention' : undefined}
        >
          <Copy size={13} />
          Copy
        </button>
        {isImage
          ? clip.has_ocr_text && (
              <button type="button" className={actionClass} onClick={() => onCopyOcrText(clip.id)}>
                <Type size={13} />
                Copy text
              </button>
            )
          : null}
        {!isImage && (
          <button type="button" className={actionClass} onClick={() => onCopy(clip.id, true)}>
            <Type size={13} />
            Copy as plain text
          </button>
        )}
        <button type="button" className={actionClass} onClick={() => onTogglePin(clip.id)}>
          <Pin size={13} className={clsx(clip.is_pinned && 'fill-current text-primary')} />
          {clip.is_pinned ? 'Unpin' : 'Pin'}
        </button>
        <button
          type="button"
          className={clsx(actionClass, 'ml-auto text-destructive')}
          onClick={() => onDelete(clip.id)}
        >
          <Trash2 size={13} />
          Delete
        </button>
      </div>
    </div>
  );
}
