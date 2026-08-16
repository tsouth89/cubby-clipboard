/**
 * Text gathering for the History window's bulk Copy (SBS-829).
 *
 * The rows in that list come from `get_clips` with `previewOnly: true`, so a
 * text row's `content` is an empty string and its `preview` is a truncated
 * prefix. Neither is the clip. Every text clip's body is therefore fetched by
 * uuid, and the row is deliberately never used as a fallback: copying the
 * prefix under a "Copied 3 clips" toast is worse than reporting the failure.
 *
 * Kept pure and injection-based so the rules can be tested without a DOM or a
 * Tauri host: the caller passes the loader.
 */

import { ClipboardItem } from '../types';

export interface BulkCopyPlan {
  /** Bodies to join, in the order the rows were selected. */
  parts: string[];
  /** Clips that had no text to contribute (images, blank text). */
  skipped: number;
  /** Clips whose body could not be loaded. Never guessed from the row. */
  failed: number;
}

/**
 * Load the full text of every selected clip.
 *
 * Images contribute nothing — bulk Copy concatenates text, and an image has
 * none. `loadBody` is `get_clip_details(...).content` in the app.
 */
export async function collectBulkCopyText(
  clips: readonly ClipboardItem[],
  loadBody: (id: string) => Promise<string>
): Promise<BulkCopyPlan> {
  // In parallel: a bulk copy of twenty large dumps is exactly the case this
  // fetch was added for, and one round trip per row in series is felt.
  const loaded = await Promise.all(
    clips.map(async (clip) => {
      if (clip.clip_type === 'image') return { kind: 'empty' as const };
      try {
        return { kind: 'text' as const, text: await loadBody(clip.id) };
      } catch (error) {
        console.error('Failed to load a clip for bulk copy:', error);
        return { kind: 'failed' as const };
      }
    })
  );

  const parts: string[] = [];
  let skipped = 0;
  let failed = 0;
  for (const result of loaded) {
    if (result.kind === 'failed') failed += 1;
    else if (result.kind === 'text' && result.text.trim()) parts.push(result.text);
    else skipped += 1;
  }
  return { parts, skipped, failed };
}
