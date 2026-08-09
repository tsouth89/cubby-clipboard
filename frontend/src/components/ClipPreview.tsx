import { useEffect, useMemo, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { clsx } from 'clsx';
import { format } from 'date-fns';
import {
  Check,
  Clock,
  Copy,
  ExternalLink,
  Pencil,
  Pin,
  ScanText,
  StickyNote,
  Trash2,
  Type,
} from 'lucide-react';
import { ClipboardItem } from '../types';
import { NOTE_CHAR_LIMIT } from '../constants';
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
  /** Save a corrected reading of an image's recognized text. */
  onSaveOcrText: (clipId: string, text: string) => Promise<void>;
  /** Queue another OCR pass for an image that has no recognized text yet. */
  onRescanOcr: (clipId: string) => Promise<void>;
  /** Copy arbitrary text — used for the corrected reading. */
  onCopyText: (text: string) => void;
  /** Save edited text back to the clip. Text clips only. */
  onSaveText: (clipId: string, text: string) => Promise<void>;
  /** Save (or clear, when empty) the clip's note. */
  onSaveNotes: (clipId: string, notes: string) => Promise<void>;
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
  onSaveOcrText,
  onRescanOcr,
  onCopyText,
  onSaveText,
  onSaveNotes,
  onTogglePin,
  onDelete,
}: ClipPreviewProps) {
  // Null when not editing; otherwise the working copy of the text.
  const [draft, setDraft] = useState<string | null>(null);
  // Working copy of the recognized text while the scan panel is open.
  const [scanDraft, setScanDraft] = useState<string | null>(null);
  const [scanBusy, setScanBusy] = useState(false);
  const [saving, setSaving] = useState(false);
  // Local working copy of the note so typing stays responsive; saved on blur.
  const [noteDraft, setNoteDraft] = useState('');
  // Set synchronously by Escape so the blur it triggers can tell a cancel from
  // an ordinary focus loss. A ref, not state, because blur runs before a
  // re-render would deliver the new value.
  const noteCancelled = useRef(false);
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
        if (!active) return;
        setDetails(loaded);
        // Each attempt owns the outcome. A refetch that succeeds after a failed
        // first load must retire the error, or the pane shows the loaded image
        // under a banner saying it could not be loaded.
        setDetailsError(null);
      })
      .catch((error) => {
        console.error('Failed to load clip details:', error);
        if (active) setDetailsError(String(error));
      });

    return () => {
      active = false;
    };
  }, [clipId, ocrReady]);

  // Abandon an unsaved edit when the selection moves — silently carrying a
  // draft onto a different clip and saving it there would be destructive.
  useEffect(() => {
    setDraft(null);
    // A correction typed for one image must never be saved onto another.
    setScanDraft(null);
  }, [clipId]);

  // Close the panel when the selection moves; a correction typed for one image
  // must never be saved onto another.
  useEffect(() => {
    setScanDraft(null);
  }, [clipId]);
  // Follow the selection rather than the keystrokes, so switching clips shows
  // the new clip's note instead of carrying the previous one across.
  useEffect(() => {
    noteCancelled.current = false;
    setNoteDraft(clip?.notes ?? '');
  }, [clipId, clip?.notes]);

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
          {scanDraft !== null ? (
            <div className="mt-2 shrink-0 border-t border-white/[0.07] pt-2">
              <p className="mb-1.5 text-[11px] text-muted-foreground">
                Recognized text — fix anything misread, then copy or save.
              </p>
              <textarea
                value={scanDraft}
                onChange={(event) => setScanDraft(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === 'Escape') {
                    event.stopPropagation();
                    setScanDraft(null);
                  }
                }}
                className="h-24 w-full resize-none rounded-md border border-primary/45 bg-black/20 p-2 text-[12px] leading-[18px] text-foreground outline-none"
              />
              <div className="mt-1.5 flex flex-wrap items-center gap-1.5">
                <button
                  type="button"
                  className={actionClass}
                  onClick={() => onCopyText(scanDraft)}
                  disabled={scanDraft.trim().length === 0}
                >
                  <Copy size={13} />
                  Copy this text
                </button>
                <button
                  type="button"
                  className={actionClass}
                  disabled={scanBusy || scanDraft === (details?.ocr_text ?? '')}
                  onClick={async () => {
                    setScanBusy(true);
                    try {
                      await onSaveOcrText(clip.id, scanDraft);
                      setScanDraft(null);
                    } finally {
                      setScanBusy(false);
                    }
                  }}
                  title="Also fixes what search matches for this image"
                >
                  <Check size={13} />
                  {scanBusy ? 'Saving…' : 'Save correction'}
                </button>
                <button type="button" className={actionClass} onClick={() => setScanDraft(null)}>
                  Close
                </button>
              </div>
            </div>
          ) : (
            <div className="mt-2 shrink-0 border-t border-white/[0.07] pt-2">
              <button
                type="button"
                className={actionClass}
                disabled={scanBusy}
                onClick={async () => {
                  if (details?.ocr_text) {
                    setScanDraft(details.ocr_text);
                    return;
                  }
                  // Nothing recognized yet: ask the queue for another pass.
                  setScanBusy(true);
                  try {
                    await onRescanOcr(clip.id);
                  } finally {
                    setScanBusy(false);
                  }
                }}
              >
                <ScanText size={13} />
                {details?.ocr_text ? 'Scan text' : scanBusy ? 'Scanning…' : 'Scan text'}
              </button>
            </div>
          )}
          {detailsError && (
            <p className="shrink-0 pt-2 text-[11px] text-destructive">
              Could not load the full image. Showing the list thumbnail instead.
            </p>
          )}
        </div>
      ) : (
        <div className="min-h-0 flex-1 overflow-y-auto p-4">
          {draft !== null ? (
            <textarea
              autoFocus
              value={draft}
              onChange={(event) => setDraft(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === 'Escape') {
                  event.stopPropagation();
                  setDraft(null);
                }
              }}
              className={clsx(
                'h-full min-h-[200px] w-full resize-none rounded-md border border-primary/45 bg-black/20 p-2 text-[12.5px] leading-[19px] text-foreground outline-none',
                kind === 'Code' && 'font-mono text-[12px]'
              )}
            />
          ) : (
            <pre
              className={clsx(
                'whitespace-pre-wrap break-words text-[12.5px] leading-[19px] text-foreground/95',
                kind === 'Code' && 'font-mono text-[12px]'
              )}
            >
              {text}
            </pre>
          )}
          {detailsError && (
            <p className="mt-3 text-[11px] text-destructive">
              Could not load the full clip. Showing the list preview instead.
            </p>
          )}
        </div>
      )}

      <div className="shrink-0 border-t border-white/[0.07] px-4 py-3">
        <label className="flex items-center gap-2">
          <StickyNote size={12} className="shrink-0 text-muted-foreground" />
          <input
            value={noteDraft}
            onChange={(event) => setNoteDraft(event.target.value)}
            onBlur={() => {
              // Escape blurs to commit the cancel, so the revert has to happen
              // here: setNoteDraft has not been applied by the time this runs,
              // and noteDraft still holds the text the user asked to discard.
              if (noteCancelled.current) {
                noteCancelled.current = false;
                setNoteDraft(clip.notes ?? '');
                return;
              }
              const next = noteDraft.trim();
              if (next !== (clip.notes ?? '')) void onSaveNotes(clip.id, next);
            }}
            onKeyDown={(event) => {
              if (event.key === 'Enter') event.currentTarget.blur();
              if (event.key === 'Escape') {
                event.stopPropagation();
                noteCancelled.current = true;
                event.currentTarget.blur();
              }
            }}
            maxLength={NOTE_CHAR_LIMIT}
            placeholder="Add a note to find this later"
            aria-label="Note"
            className="min-w-0 flex-1 border-b border-transparent bg-transparent pb-0.5 text-[11px] text-foreground outline-none transition-colors placeholder:text-muted-foreground focus:border-primary/45"
          />
        </label>
      </div>

      <div className="shrink-0 space-y-1 border-t border-white/[0.07] px-4 py-3">
        <MetaRow label="Source" value={label} />
        <MetaRow label="Kind" value={kind} />
        {captured && <MetaRow label="Captured" value={captured} />}
        {dimensions && <MetaRow label="Size" value={dimensions} />}
        {size && <MetaRow label={dimensions ? 'On disk' : 'Length'} value={size} />}
      </div>

      <div className="flex shrink-0 flex-wrap items-center gap-1.5 border-t border-white/[0.07] px-4 py-3">
        {draft !== null ? (
          <>
            <button
              type="button"
              className={actionClass}
              disabled={saving}
              onClick={async () => {
                setSaving(true);
                try {
                  await onSaveText(clip.id, draft);
                  setDraft(null);
                } finally {
                  setSaving(false);
                }
              }}
            >
              <Check size={13} />
              {saving ? 'Saving…' : 'Save'}
            </button>
            <button
              type="button"
              className={actionClass}
              disabled={saving}
              onClick={() => setDraft(null)}
            >
              Cancel
            </button>
          </>
        ) : (
          !isImage && (
            <button
              type="button"
              className={actionClass}
              // The full text, not the row's truncated preview — editing from a
              // preview would silently chop the clip on save.
              onClick={() => setDraft(details?.content ?? clip.content ?? '')}
              disabled={!details && !detailsError}
              title={!details && !detailsError ? 'Loading the full text…' : undefined}
            >
              <Pencil size={13} />
              Edit
            </button>
          )
        )}
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
