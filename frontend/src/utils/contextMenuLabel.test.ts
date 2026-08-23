import { describe, expect, it } from 'vitest';
import { contextMenuLabel } from './contextMenuLabel';

describe('contextMenuLabel', () => {
  it('does not announce folder or history menus as clip actions (SBS-1013)', () => {
    expect(contextMenuLabel('folder')).toBe('Folder actions');
    expect(contextMenuLabel('history')).toBe('History actions');
    expect(contextMenuLabel('card')).toBe('Clip actions');
  });
});
