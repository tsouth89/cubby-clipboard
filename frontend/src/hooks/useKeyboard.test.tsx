import { fireEvent, renderHook } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import { useKeyboard } from './useKeyboard';

function callbacks() {
  return {
    onClose: vi.fn(),
    onSearch: vi.fn(),
    onDelete: vi.fn(),
    onPin: vi.fn(),
    onNavigateUp: vi.fn(),
    onNavigateDown: vi.fn(),
    onPaste: vi.fn(),
    onPastePlainText: vi.fn(),
    onCopy: vi.fn(),
  };
}

describe('useKeyboard', () => {
  it('routes the flyout shortcuts and leaves bare letters alone', () => {
    const handlers = callbacks();
    renderHook(() => useKeyboard(handlers));

    fireEvent.keyDown(document.body, { key: 'f', ctrlKey: true });
    fireEvent.keyDown(document.body, { key: 'Delete' });
    fireEvent.keyDown(document.body, { key: 'p' });
    fireEvent.keyDown(document.body, { key: 'p', ctrlKey: true });
    fireEvent.keyDown(document.body, { key: 'ArrowUp' });
    fireEvent.keyDown(document.body, { key: 'ArrowDown' });
    fireEvent.keyDown(document.body, { key: 'Enter', shiftKey: true });
    fireEvent.keyDown(document.body, { key: 'Enter', ctrlKey: true });
    fireEvent.keyDown(document.body, { key: 'Enter' });

    expect(handlers.onSearch).toHaveBeenCalledOnce();
    expect(handlers.onDelete).toHaveBeenCalledOnce();
    expect(handlers.onPin).toHaveBeenCalledOnce();
    expect(handlers.onNavigateUp).toHaveBeenCalledOnce();
    expect(handlers.onNavigateDown).toHaveBeenCalledOnce();
    expect(handlers.onPastePlainText).toHaveBeenCalledOnce();
    expect(handlers.onCopy).toHaveBeenCalledOnce();
    expect(handlers.onPaste).toHaveBeenCalledOnce();
  });

  it('does not run editing shortcuts from a non-search input', () => {
    const handlers = callbacks();
    renderHook(() => useKeyboard(handlers));
    const input = document.createElement('input');
    document.body.append(input);

    fireEvent.keyDown(input, { key: 'Escape' });
    fireEvent.keyDown(input, { key: 'Delete' });
    fireEvent.keyDown(input, { key: 'ArrowDown' });
    fireEvent.keyDown(input, { key: 'Enter' });

    expect(handlers.onClose).not.toHaveBeenCalled();
    expect(handlers.onDelete).not.toHaveBeenCalled();
    expect(handlers.onNavigateDown).not.toHaveBeenCalled();
    expect(handlers.onPaste).not.toHaveBeenCalled();
  });

  it('keeps navigation active in the search input', () => {
    const handlers = callbacks();
    renderHook(() => useKeyboard(handlers));
    const search = document.createElement('input');
    search.dataset.el = 'search-input';
    document.body.append(search);

    fireEvent.keyDown(search, { key: 'Escape' });
    fireEvent.keyDown(search, { key: 'ArrowDown' });

    expect(handlers.onClose).toHaveBeenCalledOnce();
    expect(handlers.onNavigateDown).toHaveBeenCalledOnce();
  });

  it('leaves clip shortcuts to focused buttons and selects', () => {
    const handlers = callbacks();
    renderHook(() => useKeyboard(handlers));
    const button = document.createElement('button');
    const select = document.createElement('select');
    document.body.append(button, select);

    fireEvent.keyDown(button, { key: 'Enter' });
    fireEvent.keyDown(button, { key: 'Delete' });
    fireEvent.keyDown(select, { key: 'ArrowDown' });
    fireEvent.keyDown(select, { key: 'Home' });

    expect(handlers.onPaste).not.toHaveBeenCalled();
    expect(handlers.onDelete).not.toHaveBeenCalled();
    expect(handlers.onNavigateDown).not.toHaveBeenCalled();
  });

  it('ignores composition and non-arrow repeats', () => {
    const handlers = callbacks();
    renderHook(() => useKeyboard(handlers));

    fireEvent.keyDown(document.body, { key: 'Enter', isComposing: true });
    fireEvent.keyDown(document.body, { key: 'Enter', repeat: true });
    fireEvent.keyDown(document.body, { key: 'ArrowDown', repeat: true });

    expect(handlers.onPaste).not.toHaveBeenCalled();
    expect(handlers.onNavigateDown).toHaveBeenCalledOnce();
  });
});
