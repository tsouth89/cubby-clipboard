import { describe, expect, it, vi } from 'vitest';
import {
  addNonPassiveWheelListener,
  applyImageViewerWheel,
  MAX_SCALE,
  MIN_SCALE,
  NON_PASSIVE_WHEEL,
  WHEEL_ZOOM_FACTOR,
} from './imageViewerWheel';

/** A wheel event whose preventDefault we can assert. */
function wheel(partial: {
  shiftKey?: boolean;
  ctrlKey?: boolean;
  deltaX?: number;
  deltaY?: number;
  clientX?: number;
  clientY?: number;
}) {
  return {
    shiftKey: false,
    ctrlKey: false,
    deltaX: 0,
    deltaY: 0,
    clientX: 0,
    clientY: 0,
    preventDefault: vi.fn(),
    ...partial,
  };
}

function viewport(scrollLeft = 0) {
  return {
    scrollLeft,
    getBoundingClientRect: () => ({ left: 10, top: 20 }),
  };
}

describe('NON_PASSIVE_WHEEL / addNonPassiveWheelListener', () => {
  /**
   * React 19's onWheel is registered as { passive: true }, so preventDefault
   * is a no-op and Shift+wheel pans twice while Ctrl+wheel jumps (SBS-1011).
   */
  it('registers wheel as { passive: false } so preventDefault can cancel native scroll', () => {
    expect(NON_PASSIVE_WHEEL).toEqual({ passive: false });

    const target = {
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
    };
    const listener = vi.fn();
    const stop = addNonPassiveWheelListener(target as unknown as EventTarget, listener);

    expect(target.addEventListener).toHaveBeenCalledWith('wheel', listener, { passive: false });
    stop();
    expect(target.removeEventListener).toHaveBeenCalledWith('wheel', listener);
  });
});

describe('applyImageViewerWheel', () => {
  it('Shift+wheel preventDefaults and pans with deltaY so native horizontal scroll does not also run', () => {
    const event = wheel({ shiftKey: true, deltaY: 40 });
    const element = viewport(100);
    const zoom = vi.fn();

    applyImageViewerWheel(event, element, 1, zoom);

    expect(event.preventDefault).toHaveBeenCalledTimes(1);
    expect(element.scrollLeft).toBe(140);
    expect(zoom).not.toHaveBeenCalled();
  });

  it('Shift+wheel with only deltaX still pans when the device reports sideways', () => {
    const event = wheel({ shiftKey: true, deltaY: 0, deltaX: 15 });
    const element = viewport(0);

    applyImageViewerWheel(event, element, 1, vi.fn());

    expect(event.preventDefault).toHaveBeenCalledTimes(1);
    expect(element.scrollLeft).toBe(15);
  });

  it('Ctrl+wheel preventDefaults and zooms around the cursor instead of native page zoom', () => {
    const event = wheel({ ctrlKey: true, deltaY: -1, clientX: 60, clientY: 50 });
    const element = viewport();
    const zoom = vi.fn();

    applyImageViewerWheel(event, element, 1, zoom);

    expect(event.preventDefault).toHaveBeenCalledTimes(1);
    expect(zoom).toHaveBeenCalledWith(WHEEL_ZOOM_FACTOR, 50, 30, WHEEL_ZOOM_FACTOR);
  });

  it('clamps Ctrl+wheel zoom to the viewer limits', () => {
    const zoom = vi.fn();
    applyImageViewerWheel(wheel({ ctrlKey: true, deltaY: -1 }), viewport(), MAX_SCALE, zoom);
    expect(zoom).toHaveBeenCalledWith(MAX_SCALE, -10, -20, 1);

    zoom.mockClear();
    applyImageViewerWheel(wheel({ ctrlKey: true, deltaY: 1 }), viewport(), MIN_SCALE, zoom);
    expect(zoom).toHaveBeenCalledWith(MIN_SCALE, -10, -20, 1);
  });

  it('leaves a plain wheel alone so overflow-auto still scrolls vertically', () => {
    const event = wheel({ deltaY: 80 });
    const element = viewport(0);
    const zoom = vi.fn();

    applyImageViewerWheel(event, element, 1, zoom);

    expect(event.preventDefault).not.toHaveBeenCalled();
    expect(element.scrollLeft).toBe(0);
    expect(zoom).not.toHaveBeenCalled();
  });

  it('does not preventDefault Ctrl+wheel when scale is still unknown', () => {
    // Image not measured yet. Native Ctrl+wheel must not be swallowed with
    // nothing to zoom; that would feel like a dead control.
    const event = wheel({ ctrlKey: true, deltaY: -1 });
    applyImageViewerWheel(event, viewport(), null, vi.fn());
    expect(event.preventDefault).not.toHaveBeenCalled();
  });

  it('Shift+Ctrl+wheel zooms rather than pans, matching the previous onWheel order', () => {
    const event = wheel({ shiftKey: true, ctrlKey: true, deltaY: -1, clientX: 10, clientY: 20 });
    const element = viewport(50);
    const zoom = vi.fn();

    applyImageViewerWheel(event, element, 2, zoom);

    expect(element.scrollLeft).toBe(50);
    expect(zoom).toHaveBeenCalledTimes(1);
    expect(event.preventDefault).toHaveBeenCalledTimes(1);
  });
});
