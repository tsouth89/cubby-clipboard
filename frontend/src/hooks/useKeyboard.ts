import { useEffect } from 'react';

interface KeyboardOptions {
  onClose?: () => void;
  onSearch?: () => void;
  onDelete?: () => void;
  onPin?: () => void;
  onNavigateUp?: () => void;
  onNavigateDown?: () => void;
  onPaste?: () => void;
  onCopy?: () => void;
}

export function useKeyboard(options: KeyboardOptions) {
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement | null;
      const isEditing =
        target?.tagName === 'INPUT' || target?.tagName === 'TEXTAREA' || target?.isContentEditable;

      if (e.key === 'Escape' && options.onClose) {
        e.preventDefault();
        options.onClose();
      }

      if ((e.metaKey || e.ctrlKey) && e.key === 'f' && options.onSearch) {
        e.preventDefault();
        options.onSearch();
      }

      if (!isEditing && e.key === 'Delete' && options.onDelete) {
        e.preventDefault();
        options.onDelete();
      }

      if (!isEditing && e.key === 'p' && !e.metaKey && !e.ctrlKey && options.onPin) {
        e.preventDefault();
        options.onPin();
      }

      if (!isEditing && e.key === 'ArrowUp' && options.onNavigateUp) {
        e.preventDefault();
        options.onNavigateUp();
      }

      if (!isEditing && e.key === 'ArrowDown' && options.onNavigateDown) {
        e.preventDefault();
        options.onNavigateDown();
      }

      if (!isEditing && e.key === 'Enter' && (e.ctrlKey || e.metaKey) && options.onCopy) {
        e.preventDefault();
        options.onCopy();
      } else if (!isEditing && e.key === 'Enter' && options.onPaste) {
        e.preventDefault();
        options.onPaste();
      }
    };

    document.addEventListener('keydown', handleKeyDown);
    return () => document.removeEventListener('keydown', handleKeyDown);
  }, [options]);
}
