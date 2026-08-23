import { ChevronDown } from 'lucide-react';
import { clsx } from 'clsx';
import { twMerge } from 'tailwind-merge';

export interface SelectOption {
  label: string;
  value: string;
}

interface SelectProps {
  ariaLabel: string;
  value: string;
  onChange: (value: string) => void;
  options: SelectOption[];
  placeholder?: string;
  className?: string;
  disabled?: boolean;
}

export function Select({
  ariaLabel,
  value,
  onChange,
  options,
  placeholder,
  className,
  disabled,
}: SelectProps) {
  const hasSelectedOption = options.some((option) => option.value === value);

  return (
    <div className={twMerge('relative w-full', className)}>
      <select
        aria-label={ariaLabel}
        value={hasSelectedOption ? value : ''}
        onChange={(event) => onChange(event.target.value)}
        disabled={disabled}
        className={clsx(
          'w-full appearance-none rounded-lg border border-border bg-input px-3 py-2 pr-9 text-sm text-foreground transition-all duration-200 focus:outline-none focus:ring-2 focus:ring-ring',
          disabled && 'cursor-not-allowed opacity-50',
          !hasSelectedOption && 'text-muted-foreground'
        )}
      >
        {!hasSelectedOption && (
          <option value="" disabled>
            {placeholder || 'Select...'}
          </option>
        )}
        {options.map((option) => (
          <option key={option.value} value={option.value}>
            {option.label}
          </option>
        ))}
      </select>
      <ChevronDown
        aria-hidden="true"
        size={16}
        className="pointer-events-none absolute right-3 top-1/2 -translate-y-1/2 text-muted-foreground"
      />
    </div>
  );
}
