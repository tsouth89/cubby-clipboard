import { useState, useRef, useEffect, useLayoutEffect, type CSSProperties } from 'react';
import { createPortal } from 'react-dom';
import { ChevronDown, Check } from 'lucide-react';
import { clsx } from 'clsx';
import { twMerge } from 'tailwind-merge';

export interface SelectOption {
  label: string;
  value: string;
}

interface SelectProps {
  value: string;
  onChange: (value: string) => void;
  options: SelectOption[];
  placeholder?: string;
  className?: string;
  disabled?: boolean;
}

export function Select({
  value,
  onChange,
  options,
  placeholder,
  className,
  disabled,
}: SelectProps) {
  const [isOpen, setIsOpen] = useState(false);
  const [menuStyle, setMenuStyle] = useState<CSSProperties>({});
  const containerRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);

  const selectedOption = options.find((opt) => opt.value === value);

  // Position the menu against the trigger in viewport coordinates. The menu is
  // portaled to <body>, so it is never clipped by an ancestor's overflow (the
  // settings cards use overflow-hidden and the panel is a scroll container).
  const updatePosition = () => {
    const trigger = triggerRef.current;
    if (!trigger) return;
    const rect = trigger.getBoundingClientRect();
    setMenuStyle({
      position: 'fixed',
      left: rect.left,
      top: rect.bottom + 4,
      width: rect.width,
    });
  };

  useLayoutEffect(() => {
    if (isOpen) updatePosition();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [isOpen]);

  useEffect(() => {
    if (!isOpen) return;

    const handleClickOutside = (event: MouseEvent) => {
      const target = event.target as Node;
      if (containerRef.current?.contains(target)) return;
      if (menuRef.current?.contains(target)) return;
      setIsOpen(false);
    };
    // The menu is fixed-positioned, so close it if the page scrolls or resizes
    // rather than letting it detach from the trigger.
    const close = () => setIsOpen(false);

    document.addEventListener('mousedown', handleClickOutside);
    window.addEventListener('scroll', close, true);
    window.addEventListener('resize', close);

    return () => {
      document.removeEventListener('mousedown', handleClickOutside);
      window.removeEventListener('scroll', close, true);
      window.removeEventListener('resize', close);
    };
  }, [isOpen]);

  const handleSelect = (newValue: string) => {
    onChange(newValue);
    setIsOpen(false);
  };

  return (
    <div className={twMerge('relative w-full', className)} ref={containerRef}>
      <button
        ref={triggerRef}
        type="button"
        onClick={() => !disabled && setIsOpen(!isOpen)}
        disabled={disabled}
        className={clsx(
          'flex w-full items-center justify-between rounded-lg border border-border bg-input px-3 py-2 text-sm text-foreground transition-all duration-200 focus:outline-none focus:ring-2 focus:ring-ring',
          disabled && 'cursor-not-allowed opacity-50',
          isOpen && 'border-ring ring-2 ring-ring'
        )}
      >
        <span className={clsx(!selectedOption && 'text-muted-foreground')}>
          {selectedOption ? selectedOption.label : placeholder || 'Select...'}
        </span>
        <ChevronDown
          size={16}
          className={clsx(
            'text-muted-foreground transition-transform duration-200',
            isOpen && 'rotate-180'
          )}
        />
      </button>

      {isOpen &&
        createPortal(
          <div
            ref={menuRef}
            style={menuStyle}
            className="animate-in fade-in-0 zoom-in-95 z-[100] overflow-hidden rounded-lg border border-border bg-popover text-popover-foreground shadow-md duration-100"
          >
            <div className="max-h-60 overflow-y-auto py-1">
              {options.map((option) => (
                <button
                  key={option.value}
                  type="button"
                  onClick={() => handleSelect(option.value)}
                  className={clsx(
                    'relative flex w-full cursor-default select-none items-center py-1.5 pl-3 pr-8 text-sm outline-none transition-colors hover:bg-accent hover:text-accent-foreground',
                    option.value === value && 'bg-accent/50 font-medium text-accent-foreground'
                  )}
                >
                  <span className="truncate">{option.label}</span>
                  {option.value === value && (
                    <span className="absolute right-2 flex h-3.5 w-3.5 items-center justify-center">
                      <Check size={14} className="text-primary" />
                    </span>
                  )}
                </button>
              ))}
            </div>
          </div>,
          document.body
        )}
    </div>
  );
}
