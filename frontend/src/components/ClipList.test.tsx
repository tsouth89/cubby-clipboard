import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import type { ComponentProps } from 'react';
import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest';
import type { ClipboardItem } from '../types';
import { ClipList } from './ClipList';

const clips: ClipboardItem[] = [
  {
    id: 'first',
    clip_type: 'text',
    content: 'First clip',
    preview: 'First clip',
    folder_id: null,
    is_pinned: false,
    created_at: '2026-08-23T12:00:00Z',
    source_app: 'Notepad',
    source_icon: null,
    metadata: null,
    ocr_match: null,
  },
  {
    id: 'second',
    clip_type: 'text',
    content: 'Second clip',
    preview: 'Second clip',
    folder_id: null,
    is_pinned: false,
    created_at: '2026-08-23T12:01:00Z',
    source_app: 'Notepad',
    source_icon: null,
    metadata: null,
    ocr_match: null,
  },
];

beforeAll(() => {
  HTMLElement.prototype.scrollTo = vi.fn();
  HTMLElement.prototype.scrollIntoView = vi.fn();
});

afterEach(cleanup);

function listProps(overrides: Partial<ComponentProps<typeof ClipList>> = {}) {
  return {
    clips,
    isLoading: false,
    hasMore: false,
    resetToken: 0,
    density: 'comfortable' as const,
    selectedClipId: 'first',
    loadError: false,
    emptyTitle: 'No clips',
    emptyDescription: 'Copy something.',
    onSelectClip: vi.fn(),
    onPaste: vi.fn(),
    onCopy: vi.fn(),
    onTogglePin: vi.fn(),
    onLoadMore: vi.fn(),
    onRetry: vi.fn(),
    keyboardNavigation: true,
    ...overrides,
  };
}

describe('ClipList History focus model', () => {
  it('owns focus and exposes the selected option as its active descendant', () => {
    const props = listProps();
    const { rerender } = render(<ClipList {...props} />);
    const listbox = screen.getByRole('listbox', { name: 'Clipboard history' });

    expect(listbox).toHaveAttribute('tabindex', '0');
    expect(listbox).toHaveAttribute('aria-activedescendant', 'clip-option-first');
    expect(screen.getAllByRole('option')[0]).toHaveAttribute('aria-posinset', '1');

    listbox.focus();
    rerender(
      <ClipList
        {...props}
        clips={[...clips, { ...clips[1], id: 'third', content: 'Third clip' }]}
        selectedClipId="second"
        hasMore
      />
    );

    expect(document.activeElement).toBe(listbox);
    expect(listbox).toHaveAttribute('aria-activedescendant', 'clip-option-second');

    rerender(<ClipList {...props} clips={[clips[1]]} selectedClipId="second" resetToken={1} />);
    expect(document.activeElement).toBe(listbox);
    expect(listbox).toHaveAttribute('aria-activedescendant', 'clip-option-second');
  });

  it('moves selection with arrows only while the listbox owns the key', () => {
    const onSelectClip = vi.fn();
    render(
      <ClipList {...listProps({ onSelectClip, selectable: true, checkedIds: new Set<string>() })} />
    );
    const listbox = screen.getByRole('listbox');

    fireEvent.keyDown(listbox, { key: 'ArrowDown' });
    expect(onSelectClip).toHaveBeenCalledWith('second');

    onSelectClip.mockClear();
    fireEvent.keyDown(screen.getAllByRole('checkbox')[0], { key: 'ArrowDown' });
    expect(onSelectClip).not.toHaveBeenCalled();
  });

  it('keeps mouse and modifier multi-select gestures unchanged', () => {
    const onPaste = vi.fn();
    const onToggleSelect = vi.fn();
    render(
      <ClipList
        {...listProps({
          onPaste,
          selectable: true,
          checkedIds: new Set<string>(),
          onToggleSelect,
        })}
      />
    );
    const listbox = screen.getByRole('listbox');
    const firstOption = screen.getAllByRole('option')[0];

    fireEvent.mouseDown(firstOption);
    fireEvent.click(firstOption);
    expect(document.activeElement).toBe(listbox);
    expect(onPaste).toHaveBeenCalledOnce();

    fireEvent.click(firstOption, { ctrlKey: true });
    expect(onToggleSelect).toHaveBeenCalledWith('first', 0, expect.anything());
  });
});
