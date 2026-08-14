import { describe, expect, it } from 'vitest';
import { folderSelectionAfterReload } from './folderSelection';

describe('folderSelectionAfterReload', () => {
  it('keeps the selected folder when it is still in the list', () => {
    expect(
      folderSelectionAfterReload('receipts', [{ id: 'receipts' }, { id: 'work' }])
    ).toBe('receipts');
  });

  it('resets to All when the selected folder was deleted', () => {
    expect(folderSelectionAfterReload('receipts', [{ id: 'work' }])).toBe(null);
  });

  it('leaves All selected', () => {
    expect(folderSelectionAfterReload(null, [{ id: 'work' }])).toBe(null);
  });
});
