import { describe, expect, it, vi } from 'vitest';
import { collectBulkCopyText } from './bulkCopy';
import { ClipboardItem } from '../types';

/** A row as `get_clips` with `previewOnly: true` ships it: no body, prefix only. */
function previewOnlyRow(id: string, preview: string, clip_type = 'text'): ClipboardItem {
  return {
    id,
    clip_type,
    content: '',
    preview,
    folder_id: null,
    is_pinned: false,
    created_at: '2026-08-15T09:00:00Z',
    source_app: null,
    source_icon: null,
    metadata: null,
    ocr_match: null,
  };
}

describe('collectBulkCopyText', () => {
  it('copies the full bodies of preview_only text rows, not an empty error', async () => {
    const bodies: Record<string, string> = {
      one: `${'copied log line\n'.repeat(200)}FULL-BODY-ONE`,
      two: 'the whole second clip',
    };
    const rows = [previewOnlyRow('one', 'copied log line'), previewOnlyRow('two', 'the whole')];

    const plan = await collectBulkCopyText(rows, async (id) => bodies[id]);

    expect(plan.parts).toEqual([bodies.one, bodies.two]);
    expect(plan.skipped).toBe(0);
    expect(plan.failed).toBe(0);
    // The failure this replaces: both rows treated as empty, nothing to copy.
    expect(plan.parts.length).toBeGreaterThan(0);
  });

  it('never falls back to the row, so a prefix cannot be copied as the clip', async () => {
    const rows = [previewOnlyRow('one', 'copied log line')];
    const plan = await collectBulkCopyText(rows, async () => {
      throw new Error('details unavailable');
    });

    expect(plan.parts).toEqual([]);
    expect(plan.failed).toBe(1);
    expect(plan.skipped).toBe(0);
  });

  it('keeps the loaded clips when only some fail', async () => {
    const rows = [previewOnlyRow('good', 'first'), previewOnlyRow('bad', 'second')];
    const plan = await collectBulkCopyText(rows, async (id) => {
      if (id === 'bad') throw new Error('details unavailable');
      return 'first body';
    });

    expect(plan.parts).toEqual(['first body']);
    expect(plan.failed).toBe(1);
  });

  it('skips images without fetching them and counts blank text as skipped', async () => {
    const rows = [
      previewOnlyRow('shot', 'Screenshot', 'image'),
      previewOnlyRow('blank', ''),
      previewOnlyRow('real', 'text'),
    ];
    const loadBody = vi.fn(async (id: string) => (id === 'blank' ? '   ' : 'real body'));

    const plan = await collectBulkCopyText(rows, loadBody);

    expect(plan.parts).toEqual(['real body']);
    expect(plan.skipped).toBe(2);
    expect(loadBody).not.toHaveBeenCalledWith('shot');
  });

  it('preserves the selected order regardless of which load resolves first', async () => {
    const rows = [previewOnlyRow('slow', 'a'), previewOnlyRow('fast', 'b')];
    const plan = await collectBulkCopyText(rows, async (id) => {
      if (id === 'slow') await new Promise((resolve) => setTimeout(resolve, 5));
      return `${id} body`;
    });

    expect(plan.parts).toEqual(['slow body', 'fast body']);
  });
});
