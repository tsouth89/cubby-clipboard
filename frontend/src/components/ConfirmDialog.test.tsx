import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import type { ComponentProps } from 'react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { ConfirmDialog } from './ConfirmDialog';

function renderDialog(overrides: Partial<ComponentProps<typeof ConfirmDialog>> = {}) {
  const onConfirm = vi.fn();
  const onCancel = vi.fn();
  render(
    <ConfirmDialog
      isOpen
      title="Delete 2 clips?"
      message="Deleted clips cannot be recovered."
      confirmText="Delete 2 clips"
      cancelText="Cancel"
      onConfirm={onConfirm}
      onCancel={onCancel}
      {...overrides}
    />
  );
  return { onConfirm, onCancel };
}

afterEach(cleanup);

describe('ConfirmDialog', () => {
  it('reports the exact action and supports cancel and confirm', () => {
    const { onConfirm, onCancel } = renderDialog();

    expect(screen.getByRole('dialog')).toHaveAccessibleName('Delete 2 clips?');
    expect(screen.getByRole('dialog')).toHaveAccessibleDescription(
      'Deleted clips cannot be recovered.'
    );
    fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    fireEvent.click(screen.getByRole('button', { name: 'Delete 2 clips' }));

    expect(onCancel).toHaveBeenCalledOnce();
    expect(onConfirm).toHaveBeenCalledOnce();
  });

  it('blocks repeated actions while busy', () => {
    const { onConfirm, onCancel } = renderDialog({ isBusy: true });
    const confirm = screen.getByRole('button', { name: 'Delete 2 clips' });
    const cancel = screen.getByRole('button', { name: 'Cancel' });

    expect(confirm).toBeDisabled();
    expect(cancel).toBeDisabled();
    fireEvent.click(confirm);
    fireEvent.click(cancel);

    expect(onConfirm).not.toHaveBeenCalled();
    expect(onCancel).not.toHaveBeenCalled();
  });
});
