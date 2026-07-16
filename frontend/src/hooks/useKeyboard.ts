import { useEffect, useRef } from 'react';

interface KeyboardOptions {
  onClose?: () => void;
  onSearch?: () => void;
  onDelete?: () => void;
  onPin?: () => void;
  onNavigateUp?: () => void;
  onNavigateDown?: () => void;
  onPaste?: () => void;
  onPastePlainText?: () => void;
  onCopy?: () => void;
}

export function useKeyboard(options: KeyboardOptions) {
  const optionsRef = useRef(options);

  useEffect(() => {
    optionsRef.current = options;
  }, [options]);

  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.isComposing) return;

      const current = optionsRef.current;
      const target = e.target as HTMLElement | null;
      const isEditing =
        target?.tagName === 'INPUT' || target?.tagName === 'TEXTAREA' || target?.isContentEditable;
      const isSearchInput = target?.matches('[data-el="search-input"]') ?? false;
      const canNavigateHistory = !isEditing || isSearchInput;
      const isRepeatableAction = e.key === 'ArrowUp' || e.key === 'ArrowDown';

      if (e.repeat && !isRepeatableAction) return;

      if (e.key === 'Escape' && canNavigateHistory && current.onClose) {
        e.preventDefault();
        current.onClose();
        return;
      }

      if ((e.metaKey || e.ctrlKey) && e.key === 'f' && current.onSearch) {
        e.preventDefault();
        current.onSearch();
        return;
      }

      if (!isEditing && e.key === 'Delete' && current.onDelete) {
        e.preventDefault();
        current.onDelete();
        return;
      }

      if (!isEditing && e.key === 'p' && !e.metaKey && !e.ctrlKey && current.onPin) {
        e.preventDefault();
        current.onPin();
        return;
      }

      if (canNavigateHistory && e.key === 'ArrowUp' && current.onNavigateUp) {
        e.preventDefault();
        current.onNavigateUp();
        return;
      }

      if (canNavigateHistory && e.key === 'ArrowDown' && current.onNavigateDown) {
        e.preventDefault();
        current.onNavigateDown();
        return;
      }

      if (canNavigateHistory && e.key === 'Enter' && e.shiftKey && current.onPastePlainText) {
        e.preventDefault();
        current.onPastePlainText();
        return;
      }

      if (canNavigateHistory && e.key === 'Enter' && (e.ctrlKey || e.metaKey) && current.onCopy) {
        e.preventDefault();
        current.onCopy();
        return;
      }

      if (canNavigateHistory && e.key === 'Enter' && current.onPaste) {
        e.preventDefault();
        current.onPaste();
      }
    };

    document.addEventListener('keydown', handleKeyDown);
    return () => document.removeEventListener('keydown', handleKeyDown);
  }, []);
}
