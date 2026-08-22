/** Fit-to-pane zoom-out floor and 1:1-past zoom-in ceiling for the image viewer. */
export const MIN_SCALE = 0.05;
export const MAX_SCALE = 8;

/** One Ctrl+wheel notch. The toolbar buttons use a coarser 1.25 step. */
export const WHEEL_ZOOM_FACTOR = 1.15;

/**
 * React 19 registers `onWheel` as `{ passive: true }` (react-dom forces
 * `wheel` / `touchstart` / `touchmove`). `preventDefault()` is then a no-op,
 * so Chromium still runs its native Shift→horizontal scroll and Ctrl+wheel
 * action alongside the viewer's own math — one notch pans twice, and a zoom
 * jumps before the cursor re-anchor (SBS-1011).
 *
 * Attach the listener yourself with this options object.
 */
export const NON_PASSIVE_WHEEL: AddEventListenerOptions = { passive: false };

/** The fields `applyImageViewerWheel` reads. Native `WheelEvent` satisfies this. */
export interface ViewerWheelEvent {
  shiftKey: boolean;
  ctrlKey: boolean;
  deltaX: number;
  deltaY: number;
  clientX: number;
  clientY: number;
  preventDefault(): void;
}

/** Scrollable viewport the handler pans and measures. */
export interface ViewerWheelViewport {
  scrollLeft: number;
  getBoundingClientRect(): { left: number; top: number };
}

/**
 * Shift+wheel (without Ctrl) pans sideways. Ctrl+wheel zooms around the
 * cursor. A plain wheel is left alone so `overflow: auto` still scrolls.
 *
 * `preventDefault` is called only on the paths that replace native behavior.
 * Once the listener is non-passive those calls actually work; calling it on
 * a plain wheel would break vertical scroll.
 */
export function applyImageViewerWheel(
  event: ViewerWheelEvent,
  element: ViewerWheelViewport,
  scale: number | null,
  zoomAroundCursor: (next: number, offsetX: number, offsetY: number, ratio: number) => void
): void {
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
  const factor = event.deltaY < 0 ? WHEEL_ZOOM_FACTOR : 1 / WHEEL_ZOOM_FACTOR;
  const next = Math.min(MAX_SCALE, Math.max(MIN_SCALE, scale * factor));
  const ratio = next / scale;
  zoomAroundCursor(next, offsetX, offsetY, ratio);
}

export function addNonPassiveWheelListener(
  target: EventTarget,
  listener: (event: WheelEvent) => void
): () => void {
  target.addEventListener('wheel', listener as EventListener, NON_PASSIVE_WHEEL);
  return () => target.removeEventListener('wheel', listener as EventListener);
}

/**
 * Bind the image viewer's wheel handler on `element` so `preventDefault`
 * can cancel the browser's native Shift/Ctrl+wheel actions (SBS-1011).
 */
export function bindImageViewerWheel(
  element: HTMLElement,
  getScale: () => number | null,
  zoomAroundCursor: (next: number, offsetX: number, offsetY: number, ratio: number) => void
): () => void {
  const onWheel = (event: WheelEvent) => {
    applyImageViewerWheel(event, element, getScale(), zoomAroundCursor);
  };
  return addNonPassiveWheelListener(element, onWheel);
}
