import { ClipboardItem } from '../types';

export interface BulkCopyPlan {
  parts: string[];
  skipped: number;
  hidden: number;
  failed: number;
}

export interface BulkCopyOptions {
  revealedIds?: ReadonlySet<string>;
}

export interface BulkCopyTextResult {
  id: string;
  text: string | null;
  failed: boolean;
}

const NOTHING_REVEALED: ReadonlySet<string> = new Set<string>();

/** Load selected text and recognized image text in one backend batch. */
export async function collectBulkCopyText(
  clips: readonly ClipboardItem[],
  loadBatch: (ids: string[]) => Promise<readonly BulkCopyTextResult[]>,
  options: BulkCopyOptions = {}
): Promise<BulkCopyPlan> {
  const revealedIds = options.revealedIds ?? NOTHING_REVEALED;
  const visible = clips.filter((clip) => !clip.is_hidden || revealedIds.has(clip.id));
  let rows: readonly BulkCopyTextResult[];
  try {
    rows = await loadBatch(visible.map((clip) => clip.id));
  } catch (error) {
    console.error('Failed to load clips for bulk copy:', error);
    rows = visible.map((clip) => ({ id: clip.id, text: null, failed: true }));
  }
  const byId = new Map(rows.map((row) => [row.id, row]));

  const parts: string[] = [];
  let skipped = 0;
  let hidden = 0;
  let failed = 0;
  for (const clip of clips) {
    if (clip.is_hidden && !revealedIds.has(clip.id)) {
      hidden += 1;
      continue;
    }
    const row = byId.get(clip.id);
    if (!row || row.failed) failed += 1;
    else if (row.text?.trim()) parts.push(row.text);
    else skipped += 1;
  }
  return { parts, skipped, hidden, failed };
}
