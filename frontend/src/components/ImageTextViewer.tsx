import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { clsx } from 'clsx';
import { Copy, Hand, Maximize2, Minus, Plus, TextCursorInput } from 'lucide-react';
import {
  anchorWord,
  nearestWord,
  OcrWord,
  selectedText,
  selectionBands,
  selectionRange,
} from '../utils/ocrSelection';

/** Fit the image to the pane, or a fixed number of CSS pixels per image pixel. */
type Zoom = { kind: 'fit' } | { kind: 'scale'; cssPerImagePixel: number };

const MIN_SCALE = 0.05;
const MAX_SCALE = 8;

/** One image pixel per *device* pixel: the sharpest honest view, and on a
 *  scaled display that is not the same as one CSS pixel. */
function actualSizeScale(): number {
  return 1 / (typeof window === 'undefined' ? 1 : window.devicePixelRatio || 1);
}

function startingZoom(initial: 'fit' | 'actual'): Zoom {
  return initial === 'actual'
    ? { kind: 'scale', cssPerImagePixel: actualSizeScale() }
    : { kind: 'fit' };
}

interface ImageTextViewerProps {
  src: string;
  /** Recognized words as image fractions. Empty when the clip has no layout. */
  words: OcrWord[];
  /** Retention dropped the full image, so this is the surviving thumbnail. */
  dimmed?: boolean;
  /** Where to start. The cramped preview pane fits first; the pop-out window
   *  exists precisely to show the pixels, so it starts at actual size. */
  initialZoom?: 'fit' | 'actual';
  onCopySelection: (text: string) => void;
  /** Rendered beside the hint line — the pane uses it for a pop-out button. */
  actions?: React.ReactNode;
}

/**
 * The image preview you can actually read: zoom past fit-to-pane, and drag
 * across the recognized words to select and copy just that text rather than the
 * whole recognized block.
 *
 * A screenshot is typically several times wider than the preview pane, so
 * fit-to-pane renders its text at a fraction of the captured resolution and it
 * becomes unreadable. Zoom is what makes the pixels that were always there
 * visible; the selection overlay is what makes them useful.
 */
export function ImageTextViewer({
  src,
  words,
  dimmed,
  initialZoom = 'fit',
  onCopySelection,
  actions,
}: ImageTextViewerProps) {
  const viewportRef = useRef<HTMLDivElement>(null);
  const surfaceRef = useRef<HTMLDivElement>(null);
  const imgRef = useRef<HTMLImageElement>(null);
  const [zoom, setZoom] = useState<Zoom>(() => startingZoom(initialZoom));
  const [natural, setNatural] = useState<{ width: number; height: number } | null>(null);
  const [viewport, setViewport] = useState({ width: 0, height: 0 });
  const [anchor, setAnchor] = useState<number | null>(null);
  const [focus, setFocus] = useState<number | null>(null);
  const [panMode, setPanMode] = useState(false);
  const dragging = useRef(false);
  // Scroll position and pointer origin captured at the start of a pan.
  const panning = useRef<{
    pointerX: number;
    pointerY: number;
    scrollLeft: number;
    scrollTop: number;
  } | null>(null);

  // Reset per image: a new clip should never inherit the previous one's zoom or
  // a selection whose indices point into a different word list.
  //
  // The size has to be read here as well as in `onLoad`. The list row already
  // rendered this exact data URL, so the image is usually decoded by the time
  // React attaches the handler and no load event ever fires — leaving the
  // viewer with no dimensions and the image splashed out at full resolution.
  useEffect(() => {
    setZoom(startingZoom(initialZoom));
    setAnchor(null);
    setFocus(null);
    setPanMode(false);
    dragging.current = false;
    panning.current = null;
    // A new image starts at its own top-left. Inheriting the previous one's
    // scroll can open it on blank space well past its edge.
    if (viewportRef.current) {
      viewportRef.current.scrollLeft = 0;
      viewportRef.current.scrollTop = 0;
    }

    const image = imgRef.current;
    setNatural(
      image?.complete && image.naturalWidth > 0
        ? { width: image.naturalWidth, height: image.naturalHeight }
        : null
    );
  }, [initialZoom, src]);

  useLayoutEffect(() => {
    const element = viewportRef.current;
    if (!element) return;
    const observer = new ResizeObserver(([entry]) => {
      setViewport({
        width: entry.contentRect.width,
        height: entry.contentRect.height,
      });
    });
    observer.observe(element);
    return () => observer.disconnect();
  }, []);

  // Recomputed per render, and read afresh inside handlers, so moving the
  // window to a monitor with a different scale factor doesn't leave 1:1 and the
  // percentage label quoting the old ratio.
  const actualScale = actualSizeScale();

  const fitScale = useMemo(() => {
    if (!natural || viewport.width === 0 || viewport.height === 0) return null;
    return Math.min(viewport.width / natural.width, viewport.height / natural.height);
  }, [natural, viewport]);

  const scale = zoom.kind === 'fit' ? fitScale : zoom.cssPerImagePixel;
  const display = useMemo(() => {
    if (!natural || scale === null) return null;
    return { width: natural.width * scale, height: natural.height * scale };
  }, [natural, scale]);

  const range = selectionRange(anchor, focus);
  const selection = useMemo(() => selectedText(words, range), [words, range]);
  const bands = useMemo(() => selectionBands(words, range), [words, range]);

  const setScale = useCallback((next: number) => {
    setZoom({ kind: 'scale', cssPerImagePixel: Math.min(MAX_SCALE, Math.max(MIN_SCALE, next)) });
  }, []);

  const zoomBy = useCallback(
    (factor: number) => {
      if (scale === null) return;
      setScale(scale * factor);
    },
    [scale, setScale]
  );

  // Point under the pointer, as a fraction of the image.
  const pointAt = useCallback((event: React.PointerEvent) => {
    const surface = surfaceRef.current;
    if (!surface) return null;
    const rect = surface.getBoundingClientRect();
    if (rect.width === 0 || rect.height === 0) return null;
    return {
      x: (event.clientX - rect.left) / rect.width,
      y: (event.clientY - rect.top) / rect.height,
    };
  }, []);

  const startPan = useCallback((event: React.PointerEvent) => {
    const element = viewportRef.current;
    if (!element) return;
    event.preventDefault();
    (event.target as Element).setPointerCapture?.(event.pointerId);
    panning.current = {
      pointerX: event.clientX,
      pointerY: event.clientY,
      scrollLeft: element.scrollLeft,
      scrollTop: element.scrollTop,
    };
  }, []);

  const handlePointerDown = useCallback(
    (event: React.PointerEvent) => {
      // Middle button always pans, whatever the mode — the same reflex as any
      // map or image editor. Left button pans only in pan mode.
      if (event.button === 1 || (panMode && event.button === 0)) {
        startPan(event);
        return;
      }
      if (event.button !== 0) return;
      // No recognized text at all: the whole surface is "empty space", so
      // left-drag pans rather than doing nothing.
      if (words.length === 0) {
        startPan(event);
        return;
      }
      const point = pointAt(event);
      if (!point) return;
      const index = anchorWord(words, point);
      if (index === null) {
        // Nothing to select here. Clear, the way clicking beside a paragraph
        // does in a document, and let the drag move the image instead — on a
        // screenshot most of the surface is not text, so dragging the empty
        // parts is the obvious way to get around a zoomed-in image.
        setAnchor(null);
        setFocus(null);
        startPan(event);
        return;
      }
      event.preventDefault();
      dragging.current = true;
      (event.target as Element).setPointerCapture?.(event.pointerId);
      setAnchor(index);
      setFocus(index);
    },
    [panMode, pointAt, startPan, words]
  );

  const handlePointerMove = useCallback(
    (event: React.PointerEvent) => {
      const pan = panning.current;
      if (pan) {
        const element = viewportRef.current;
        if (!element) return;
        element.scrollLeft = pan.scrollLeft - (event.clientX - pan.pointerX);
        element.scrollTop = pan.scrollTop - (event.clientY - pan.pointerY);
        return;
      }
      if (!dragging.current || words.length === 0) return;
      const point = pointAt(event);
      if (!point) return;
      // Extend to the nearest word rather than only to words directly under the
      // cursor, so a sweep that runs past the end of a line still selects it.
      const index = nearestWord(words, point);
      if (index !== null) setFocus(index);
    },
    [pointAt, words]
  );

  const endDrag = useCallback(() => {
    dragging.current = false;
    panning.current = null;
  }, []);

  // Ctrl+wheel zooms around the cursor, Shift+wheel scrolls sideways, and a
  // plain wheel scrolls vertically. Sideways is explicit because a zoomed
  // screenshot is usually wider than the pane, and a mouse with only a vertical
  // wheel would otherwise have no way to reach the right-hand side.
  const handleWheel = useCallback(
    (event: React.WheelEvent) => {
      const element = viewportRef.current;
      if (!element) return;

      if (event.shiftKey && !event.ctrlKey) {
        event.preventDefault();
        element.scrollLeft += event.deltaY !== 0 ? event.deltaY : event.deltaX;
        return;
      }
      if (!event.ctrlKey || scale === null) return;
      event.preventDefault();
      const rect = element.getBoundingClientRect();
      const offsetX = event.clientX - rect.left;
      const offsetY = event.clientY - rect.top;
      const factor = event.deltaY < 0 ? 1.15 : 1 / 1.15;
      const next = Math.min(MAX_SCALE, Math.max(MIN_SCALE, scale * factor));
      const ratio = next / scale;
      setZoom({ kind: 'scale', cssPerImagePixel: next });
      // Keep the pixel under the cursor under the cursor.
      requestAnimationFrame(() => {
        element.scrollLeft = (element.scrollLeft + offsetX) * ratio - offsetX;
        element.scrollTop = (element.scrollTop + offsetY) * ratio - offsetY;
      });
    },
    [scale]
  );

  // Escape clears a selection before anything else acts on it, and Ctrl+C
  // copies it. Capture phase so the History window's own Escape-to-close and
  // Enter-to-copy bindings don't fire while a selection is live.
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (!range) return;
      // Never take keys from whatever the user is typing in. Without this the
      // capture-phase listener would swallow Escape and Ctrl+C from the search
      // input whenever an image selection happened to be live.
      const active = document.activeElement as HTMLElement | null;
      if (
        active &&
        (active.tagName === 'INPUT' || active.tagName === 'TEXTAREA' || active.isContentEditable)
      ) {
        return;
      }
      if (event.key === 'Escape') {
        event.stopPropagation();
        event.preventDefault();
        setAnchor(null);
        setFocus(null);
        return;
      }
      if (event.key === 'c' && (event.ctrlKey || event.metaKey) && selection) {
        event.stopPropagation();
        event.preventDefault();
        onCopySelection(selection);
      }
    };
    document.addEventListener('keydown', onKeyDown, true);
    return () => document.removeEventListener('keydown', onKeyDown, true);
  }, [onCopySelection, range, selection]);

  const selectable = words.length > 0;
  const controlClass =
    'rounded-md border border-white/[0.09] bg-black/50 p-1.5 text-foreground/85 backdrop-blur transition-colors hover:bg-black/70 disabled:opacity-40';

  return (
    <div className="relative flex h-full min-h-0 flex-col">
      <div
        ref={viewportRef}
        data-el="image-viewer"
        className={clsx(
          'min-h-0 flex-1 overflow-auto',
          // Fit has nothing to pan, so center it; zoomed in, the surface is
          // larger than the pane and centering would fight the scroll.
          zoom.kind === 'fit' && 'flex items-center justify-center'
        )}
        onWheel={handleWheel}
      >
        <div
          ref={surfaceRef}
          className="relative shrink-0"
          style={{
            ...(display ? { width: display.width, height: display.height } : {}),
            cursor: panMode ? 'grab' : undefined,
          }}
          onPointerDown={handlePointerDown}
          onPointerMove={handlePointerMove}
          onPointerUp={endDrag}
          onPointerCancel={endDrag}
        >
          <img
            ref={imgRef}
            src={src}
            alt=""
            draggable={false}
            onLoad={(event) =>
              setNatural({
                width: event.currentTarget.naturalWidth,
                height: event.currentTarget.naturalHeight,
              })
            }
            className={clsx('block select-none', dimmed && 'opacity-70')}
            style={{
              width: display ? display.width : undefined,
              height: display ? display.height : undefined,
              // Until the size is known, stay inside the pane. Falling back to
              // the image's natural size would splash a 2862px screenshot
              // across the window.
              maxWidth: display ? undefined : '100%',
              maxHeight: display ? undefined : '100%',
              // Magnifying past 1:1 with smoothing turns small text to mush;
              // the original pixels read better even blocky.
              imageRendering: scale !== null && scale > 1 ? 'pixelated' : 'auto',
            }}
          />

          {selectable && display && (
            <div
              data-el="ocr-word-layer"
              className="absolute inset-0"
              // In pan mode the layer must not swallow the drag as a selection.
              style={{
                cursor: panMode ? 'grab' : 'text',
                pointerEvents: panMode ? 'none' : 'auto',
              }}
              aria-hidden
            >
              {bands.map((band, index) => (
                <span
                  key={index}
                  data-el="ocr-selection-band"
                  className="absolute"
                  style={{
                    // Pad the band slightly beyond the glyph boxes so it reads
                    // as highlighted text rather than a box drawn around it.
                    left: `${band.x * 100}%`,
                    top: `${(band.y - band.height * 0.12) * 100}%`,
                    width: `${band.width * 100}%`,
                    height: `${band.height * 1.24 * 100}%`,
                    // A fixed selection blue, not the theme accent: this sits on
                    // arbitrary screenshot pixels where a themed tint can wash
                    // out to invisible. No outline — a continuous fill per line
                    // is what makes it look like a text selection.
                    background: 'rgba(51, 122, 245, 0.40)',
                    borderRadius: 2,
                  }}
                />
              ))}
            </div>
          )}
        </div>
      </div>

      <div className="pointer-events-none absolute right-3 top-3 flex items-center gap-1">
        <button
          type="button"
          className={clsx(controlClass, 'pointer-events-auto', panMode && 'text-primary')}
          onClick={() => setPanMode((on) => !on)}
          aria-pressed={panMode}
          title={
            panMode
              ? 'Pan mode: drag to move the image'
              : 'Pan mode (or drag with the middle mouse button)'
          }
          aria-label="Pan mode"
        >
          <Hand size={14} />
        </button>
        <button
          type="button"
          className={clsx(controlClass, 'pointer-events-auto')}
          onClick={() => zoomBy(1 / 1.25)}
          disabled={scale === null}
          title="Zoom out"
          aria-label="Zoom out"
        >
          <Minus size={14} />
        </button>
        <button
          type="button"
          className={clsx(controlClass, 'pointer-events-auto')}
          onClick={() => zoomBy(1.25)}
          disabled={scale === null}
          title="Zoom in"
          aria-label="Zoom in"
        >
          <Plus size={14} />
        </button>
        <button
          type="button"
          className={clsx(
            controlClass,
            'pointer-events-auto',
            zoom.kind === 'fit' && 'text-primary'
          )}
          onClick={() => setZoom({ kind: 'fit' })}
          title="Fit to pane"
          aria-label="Fit to pane"
        >
          <Maximize2 size={14} />
        </button>
        <button
          type="button"
          className={clsx(controlClass, 'pointer-events-auto px-2 text-[11px] font-medium')}
          onClick={() => setScale(actualSizeScale())}
          title="Actual size (1 image pixel per screen pixel)"
        >
          1:1
        </button>
      </div>

      <div className="flex shrink-0 items-center gap-2 px-1 pt-2 text-[11px] text-muted-foreground">
        {panMode ? (
          <>
            <Hand size={12} className="shrink-0" />
            <span>Drag to move the image · Shift+wheel scrolls sideways</span>
          </>
        ) : selectable ? (
          <>
            <TextCursorInput size={12} className="shrink-0" />
            <span>Drag across text to select it · drag anywhere else to move the image</span>
          </>
        ) : (
          <span>No recognized text to select on this image</span>
        )}
        {actions}
        {scale !== null && (
          <span className="ml-auto shrink-0 tabular-nums">
            {Math.round(scale * 100 * (1 / actualScale))}%
          </span>
        )}
      </div>

      {selection && (
        <div
          data-el="ocr-selection-bar"
          className="pointer-events-auto absolute bottom-10 left-1/2 flex -translate-x-1/2 items-center gap-2 rounded-lg border border-white/[0.12] bg-[#202023]/95 px-2 py-1.5 shadow-lg"
        >
          <span className="max-w-[280px] truncate text-[11px] text-foreground/70">
            {selection.replace(/\n/g, ' ')}
          </span>
          <button
            type="button"
            onClick={() => onCopySelection(selection)}
            className="inline-flex items-center gap-1.5 rounded-md bg-primary/90 px-2.5 py-1 text-[11px] font-medium text-white transition-colors hover:bg-primary"
          >
            <Copy size={12} />
            Copy
          </button>
        </div>
      )}
    </div>
  );
}
