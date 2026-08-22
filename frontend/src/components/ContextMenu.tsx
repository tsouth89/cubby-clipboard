import { useEffect, useLayoutEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { ContextMenuKind, contextMenuLabelKey } from '../utils/contextMenuLabel';

interface ContextMenuProps {
  x: number;
  y: number;
  options: {
    label: string;
    onClick: () => void;
    danger?: boolean;
    disabled?: boolean;
  }[];
  onClose: () => void;
  /** What this menu acts on. Drives the default accessible name so Folder and
   *  History are not announced as clip actions (SBS-1013). */
  kind: ContextMenuKind;
  /** Accessible name for the menu. Screen readers announce this on open.
   *  Defaults from `kind` rather than a clip-only literal. */
  label?: string;
}

export function ContextMenu({ x, y, options, onClose, kind, label }: ContextMenuProps) {
  const { t } = useTranslation();
  const menuRef = useRef<HTMLDivElement>(null);
  const itemRefs = useRef<(HTMLButtonElement | null)[]>([]);
  const [position, setPosition] = useState({ x, y });

  useLayoutEffect(() => {
    const menu = menuRef.current;
    if (!menu) return;

    const margin = 8;
    const rect = menu.getBoundingClientRect();
    setPosition({
      x: Math.max(margin, Math.min(x, window.innerWidth - rect.width - margin)),
      y: Math.max(margin, Math.min(y, window.innerHeight - rect.height - margin)),
    });
  }, [x, y, options.length]);

  // Indices of items a keyboard user can land on. Disabled items are skipped
  // rather than focused-and-inert, which is what a menu widget is expected to do.
  const enabledIndices = options
    .map((option, index) => (option.disabled ? -1 : index))
    .filter((index) => index >= 0);

  const focusItem = (index: number) => itemRefs.current[index]?.focus();

  useEffect(() => {
    // Move focus into the menu on open, and put it back where it came from on
    // close. Without this the menu is reachable only by tabbing through
    // everything behind it, which for a right-click menu means not at all.
    const previouslyFocused = document.activeElement as HTMLElement | null;
    if (enabledIndices.length > 0) {
      focusItem(enabledIndices[0]);
    } else {
      menuRef.current?.focus();
    }

    return () => {
      if (previouslyFocused?.isConnected) {
        previouslyFocused.focus();
      }
    };
    // Open-time behaviour only; re-running on option changes would yank focus
    // back to the first item mid-navigation.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    function handleClickOutside(event: MouseEvent) {
      if (menuRef.current && !menuRef.current.contains(event.target as Node)) {
        onClose();
      }
    }
    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === 'Escape') {
        onClose();
        return;
      }
      if (enabledIndices.length === 0) return;

      const active = document.activeElement;
      // -1 when focus is not on a menu item at all, e.g. it landed on the menu
      // container. Treat that as "no current item" and jump to the end the
      // arrow points at, instead of letting it fall out of the modulo
      // arithmetic (ArrowUp from -1 lands second-to-last, not last).
      const current = enabledIndices.indexOf(itemRefs.current.findIndex((item) => item === active));
      const last = enabledIndices.length - 1;

      switch (event.key) {
        case 'ArrowDown':
          event.preventDefault();
          focusItem(
            current === -1 ? enabledIndices[0] : enabledIndices[(current + 1) % (last + 1)]
          );
          break;
        case 'ArrowUp':
          event.preventDefault();
          focusItem(
            current === -1
              ? enabledIndices[last]
              : enabledIndices[(current - 1 + last + 1) % (last + 1)]
          );
          break;
        case 'Home':
          event.preventDefault();
          focusItem(enabledIndices[0]);
          break;
        case 'End':
          event.preventDefault();
          focusItem(enabledIndices[enabledIndices.length - 1]);
          break;
        case 'Tab':
          // A menu is a single stop, not a tab sequence: Tab dismisses it.
          // preventDefault matters -- without it the browser also advances
          // focus, which races the focus restore in the unmount cleanup and
          // makes where focus ends up depend on ordering.
          event.preventDefault();
          onClose();
          break;
      }
    }

    document.addEventListener('mousedown', handleClickOutside);
    document.addEventListener('keydown', handleKeyDown);
    return () => {
      document.removeEventListener('mousedown', handleClickOutside);
      document.removeEventListener('keydown', handleKeyDown);
    };
  }, [onClose, enabledIndices.join(',')]);

  const style = {
    top: position.y,
    left: position.x,
  };

  return (
    <div
      ref={menuRef}
      role="menu"
      aria-label={label ?? t(contextMenuLabelKey(kind))}
      tabIndex={-1}
      className="animate-in fade-in-0 zoom-in-95 fixed z-50 max-h-[min(24rem,calc(100vh-1rem))] min-w-[12rem] overflow-y-auto rounded-lg border border-white/[0.1] bg-popover/95 p-1.5 shadow-2xl backdrop-blur-xl"
      style={style}
    >
      <div className="flex flex-col">
        {options.map((option, index) => (
          <button
            key={index}
            role="menuitem"
            // Roving focus: the menu is one Tab stop and arrows move within it.
            tabIndex={-1}
            ref={(element) => {
              itemRefs.current[index] = element;
            }}
            disabled={option.disabled}
            onClick={() => {
              option.onClick();
              onClose();
            }}
            className={`relative flex cursor-default select-none items-center rounded-md px-2.5 py-2 text-left text-xs outline-none transition-colors hover:bg-accent hover:text-accent-foreground focus:bg-accent focus:text-accent-foreground disabled:pointer-events-none disabled:opacity-40 ${option.danger ? 'text-red-400 focus:text-red-400' : 'text-popover-foreground'} `}
          >
            {option.label}
          </button>
        ))}
      </div>
    </div>
  );
}
