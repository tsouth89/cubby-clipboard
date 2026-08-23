import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { useState } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { useModalDialog } from './useModalDialog';

function Harness({ onEscape = vi.fn() }: { onEscape?: () => void }) {
  const [open, setOpen] = useState(false);
  const close = () => {
    onEscape();
    setOpen(false);
  };
  const ref = useModalDialog<HTMLDivElement>(close, open);

  return (
    <>
      <button onClick={() => setOpen(true)}>Open</button>
      <button>Background</button>
      {open && (
        <div ref={ref} role="dialog" tabIndex={-1}>
          <input aria-label="Passphrase" autoFocus />
          <button onClick={close}>Cancel</button>
        </div>
      )}
    </>
  );
}

describe('useModalDialog', () => {
  beforeEach(() => {
    vi.spyOn(HTMLElement.prototype, 'offsetParent', 'get').mockReturnValue(document.body);
  });

  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
  });

  it('moves focus inside, traps Tab, and restores the opener', () => {
    render(<Harness />);
    const opener = screen.getByRole('button', { name: 'Open' });

    opener.focus();
    fireEvent.click(opener);
    const input = screen.getByRole('textbox', { name: 'Passphrase' });
    const cancel = screen.getByRole('button', { name: 'Cancel' });
    expect(input).toHaveFocus();

    cancel.focus();
    fireEvent.keyDown(cancel, { key: 'Tab' });
    expect(input).toHaveFocus();

    fireEvent.click(cancel);
    expect(opener).toHaveFocus();
  });

  it('calls the escape handler and restores focus', () => {
    const onEscape = vi.fn();
    render(<Harness onEscape={onEscape} />);
    const opener = screen.getByRole('button', { name: 'Open' });

    opener.focus();
    fireEvent.click(opener);
    fireEvent.keyDown(screen.getByRole('dialog'), { key: 'Escape' });

    expect(onEscape).toHaveBeenCalledOnce();
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    expect(opener).toHaveFocus();
  });
});
