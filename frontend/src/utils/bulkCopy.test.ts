import { describe, expect, it, vi } from 'vitest';
import { collectBulkCopyText, type BulkCopyTextResult } from './bulkCopy';
import { ClipboardItem } from '../types';

function row(id: string, clip_type = 'text'): ClipboardItem {
  return {
    id,
    clip_type,
    content: '',
    preview: '',
    folder_id: null,
    is_pinned: false,
    created_at: '2026-08-15T09:00:00Z',
    source_app: null,
    source_icon: null,
    metadata: null,
    ocr_match: null,
  };
}

const result = (id: string, text: string | null, failed = false): BulkCopyTextResult => ({
  id,
  text,
  failed,
});

describe('collectBulkCopyText', () => {
  it('loads text clips in one batch', async () => {
    const load = vi.fn(async () => [result('one', 'first body'), result('two', 'second body')]);
    const plan = await collectBulkCopyText([row('one'), row('two')], load);

    expect(load).toHaveBeenCalledOnce();
    expect(load).toHaveBeenCalledWith(['one', 'two']);
    expect(plan.parts).toEqual(['first body', 'second body']);
  });

  it('includes recognized image text in visible selection order', async () => {
    const load = vi.fn(async () => [result('text', 'plain'), result('shot', 'invoice total')]);
    const plan = await collectBulkCopyText([row('shot', 'image'), row('text')], load);

    expect(plan.parts).toEqual(['invoice total', 'plain']);
    expect(plan.skipped).toBe(0);
  });

  it('includes OCR after the full image has expired', async () => {
    const expired = { ...row('old-shot', 'image'), image_expired: true, has_ocr_text: true };
    const plan = await collectBulkCopyText([expired], async () => [
      result('old-shot', 'retained words'),
    ]);

    expect(plan.parts).toEqual(['retained words']);
  });

  it('counts only genuinely blank results as skipped', async () => {
    const plan = await collectBulkCopyText([row('shot', 'image'), row('blank')], async () => [
      result('shot', null),
      result('blank', '   '),
    ]);

    expect(plan.parts).toEqual([]);
    expect(plan.skipped).toBe(2);
    expect(plan.failed).toBe(0);
  });

  it('does not request an unrevealed hidden clip', async () => {
    const secret = { ...row('secret'), is_hidden: true };
    const load = vi.fn(async () => [result('plain', 'visible')]);
    const plan = await collectBulkCopyText([secret, row('plain')], load);

    expect(load).toHaveBeenCalledWith(['plain']);
    expect(plan.parts).toEqual(['visible']);
    expect(plan.hidden).toBe(1);
  });

  it('loads a hidden clip only after this session revealed it', async () => {
    const secret = { ...row('secret'), is_hidden: true };
    const plan = await collectBulkCopyText([secret], async () => [result('secret', 'swordfish')], {
      revealedIds: new Set(['secret']),
    });

    expect(plan.parts).toEqual(['swordfish']);
    expect(plan.hidden).toBe(0);
  });

  it('keeps good rows and reports missing or unreadable rows', async () => {
    const plan = await collectBulkCopyText([row('good'), row('bad'), row('gone')], async () => [
      result('bad', null, true),
      result('good', 'kept'),
    ]);

    expect(plan.parts).toEqual(['kept']);
    expect(plan.failed).toBe(2);
  });

  it('reports every requested row when the batch fails', async () => {
    const plan = await collectBulkCopyText([row('one'), row('two')], async () => {
      throw new Error('database unavailable');
    });

    expect(plan.parts).toEqual([]);
    expect(plan.failed).toBe(2);
  });
});
