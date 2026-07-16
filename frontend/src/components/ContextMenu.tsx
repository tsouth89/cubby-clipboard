import { useEffect, useLayoutEffect, useRef, useState } from 'react';

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
}

export function ContextMenu({ x, y, options, onClose }: ContextMenuProps) {
  const menuRef = useRef<HTMLDivElement>(null);
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

  useEffect(() => {
    function handleClickOutside(event: MouseEvent) {
      if (menuRef.current && !menuRef.current.contains(event.target as Node)) {
        onClose();
      }
    }
    // Handle Escape key
    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === 'Escape') {
        onClose();
      }
    }

    document.addEventListener('mousedown', handleClickOutside);
    document.addEventListener('keydown', handleKeyDown);
    return () => {
      document.removeEventListener('mousedown', handleClickOutside);
      document.removeEventListener('keydown', handleKeyDown);
    };
  }, [onClose]);

  const style = {
    top: position.y,
    left: position.x,
  };

  return (
    <div
      ref={menuRef}
      className="animate-in fade-in-0 zoom-in-95 fixed z-50 max-h-[min(24rem,calc(100vh-1rem))] min-w-[12rem] overflow-y-auto rounded-lg border border-white/[0.1] bg-popover/95 p-1.5 shadow-2xl backdrop-blur-xl"
      style={style}
    >
      <div className="flex flex-col">
        {options.map((option, index) => (
          <button
            key={index}
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
