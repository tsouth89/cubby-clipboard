import { useEffect, useRef } from 'react';

/**
 * Focus behaviour every `aria-modal="true"` container owes its users.
 *
 * Declaring `aria-modal` tells a screen reader the rest of the page is inert.
 * Without a Tab trap that is a lie: focus walks out into content the user was
 * just told does not exist, with no way to tell it left. So the three pieces
 * belong together rather than being optional extras.
 *
 * - move focus into the dialog on open (respecting an `autoFocus` element)
 * - keep Tab and Shift+Tab inside it
 * - return focus to whatever was focused before, on close
 *
 * `onEscape` is optional so a dialog mid-operation can decline to close, and so
 * a dialog that already owns its own Escape handling can opt out.
 *
 * `enabled` exists because a dialog that stays mounted and renders `null` while
 * closed has no container to focus at mount time. Gate on the same condition
 * that renders the dialog, or the trap silently never attaches.
 */
export function useModalDialog<T extends HTMLElement>(onEscape?: () => void, enabled = true) {
  const containerRef = useRef<T>(null);
  // Read through a ref so changing the handler between renders does not tear
  // down and re-run the focus effect, which would steal focus back on every
  // parent render.
  const escapeRef = useRef(onEscape);
  escapeRef.current = onEscape;

  useEffect(() => {
    if (!enabled) return;
    const container = containerRef.current;
    if (!container) return;

    const previouslyFocused = document.activeElement as HTMLElement | null;

    const focusable = () =>
      Array.from(
        container.querySelectorAll<HTMLElement>(
          'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])'
        )
      ).filter((element) => element.offsetParent !== null || element === document.activeElement);

    // Anything already carrying autoFocus wins; otherwise the first focusable
    // control, and failing that the container, so focus is never left outside.
    if (!container.contains(document.activeElement)) {
      const candidates = focusable();
      const preferred = candidates.find((element) => element.hasAttribute('autofocus'));
      (preferred ?? candidates[0] ?? container).focus();
    }

    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        escapeRef.current?.();
        return;
      }
      if (event.key !== 'Tab') return;

      const candidates = focusable();
      if (candidates.length === 0) {
        // Nothing to move to: hold focus on the container rather than letting
        // it escape to the background.
        event.preventDefault();
        container.focus();
        return;
      }

      const first = candidates[0];
      const last = candidates[candidates.length - 1];
      const active = document.activeElement;

      // Focus can sit outside the dialog despite the trap -- a mousedown on
      // background content moves it there without a Tab we could intercept.
      // Pull it back in on the next Tab, in whichever direction was pressed,
      // rather than letting that Tab walk further away.
      if (!container.contains(active)) {
        event.preventDefault();
        (event.shiftKey ? last : first).focus();
        return;
      }

      if (event.shiftKey && active === first) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && active === last) {
        event.preventDefault();
        first.focus();
      }
    };

    document.addEventListener('keydown', handleKeyDown, true);
    return () => {
      document.removeEventListener('keydown', handleKeyDown, true);
      // The invoking control can be gone by now (deleted clip, closed panel);
      // isConnected keeps that from throwing focus somewhere arbitrary.
      if (previouslyFocused?.isConnected) {
        previouslyFocused.focus();
      }
    };
  }, [enabled]);

  return containerRef;
}
