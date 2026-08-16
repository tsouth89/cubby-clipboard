/**
 * Text gathering for the History window's bulk Copy (SBS-829).
 *
 * The rows in that list come from `get_clips` with `previewOnly: true`, so a
 * text row's `content` is an empty string and its `preview` is a truncated
 * prefix. Neither is the clip. Every text clip's body is therefore fetched by
 * uuid, and the row is deliberately never used as a fallback: copying the
 * prefix under a "Copied 3 clips" toast is worse than reporting the failure.
 *
 * A hidden clip is the one row that is *not* fetched. Its payload only reaches
 * the webview through an explicit reveal, and bulk Copy must not be a way
 * around that.
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
  /** Hidden clips left alone because this session never revealed them. */
  hidden: number;
  /** Clips whose body could not be loaded. Never guessed from the row. */
  failed: number;
}

export interface BulkCopyOptions {
  /**
   * Clip ids the user revealed in this session. A hidden clip outside this set
   * is never loaded: `get_clip_details` returns the decrypted payload with no
   * hidden check of its own, so Select All + Copy would otherwise pull every
   * withheld secret into the webview and onto the clipboard (SBS-829).
   */
  revealedIds?: ReadonlySet<string>;
  /** Bodies loaded at once. See `BULK_COPY_CONCURRENCY`. */
  concurrency?: number;
}

/**
 * How many bodies may be in flight at once.
 *
 * Each response is a full decrypted clip, so an uncapped fan-out over a
 * Select All after load-more holds every selected dump in renderer memory at
 * the same time. Four keeps the round trips overlapped without that.
 */
export const BULK_COPY_CONCURRENCY = 4;

const NOTHING_REVEALED: ReadonlySet<string> = new Set<string>();

type LoadResult =
  { kind: 'text'; text: string } | { kind: 'empty' } | { kind: 'hidden' } | { kind: 'failed' };

/**
 * Load the full text of every selected clip.
 *
 * Images contribute nothing — bulk Copy concatenates text, and an image has
 * none. `loadBody` is `get_clip_details(...).content` in the app.
 */
export async function collectBulkCopyText(
  clips: readonly ClipboardItem[],
  loadBody: (id: string) => Promise<string>,
  options: BulkCopyOptions = {}
): Promise<BulkCopyPlan> {
  const revealedIds = options.revealedIds ?? NOTHING_REVEALED;
  const concurrency = Math.max(1, options.concurrency ?? BULK_COPY_CONCURRENCY);

  const loadOne = async (clip: ClipboardItem): Promise<LoadResult> => {
    if (clip.clip_type === 'image') return { kind: 'empty' };
    if (clip.is_hidden && !revealedIds.has(clip.id)) return { kind: 'hidden' };
    try {
      return { kind: 'text', text: await loadBody(clip.id) };
    } catch (error) {
      console.error('Failed to load a clip for bulk copy:', error);
      return { kind: 'failed' };
    }
  };

  // Bounded fan-out. Results are written by index, so the join order stays the
  // selection order no matter which load resolves first.
  const loaded: LoadResult[] = new Array(clips.length);
  let next = 0;
  const worker = async () => {
    for (let index = next++; index < clips.length; index = next++) {
      loaded[index] = await loadOne(clips[index]);
    }
  };
  await Promise.all(Array.from({ length: Math.min(concurrency, clips.length) }, () => worker()));

  const parts: string[] = [];
  let skipped = 0;
  let hidden = 0;
  let failed = 0;
  for (const result of loaded) {
    if (result.kind === 'failed') failed += 1;
    else if (result.kind === 'hidden') hidden += 1;
    else if (result.kind === 'text' && result.text.trim()) parts.push(result.text);
    else skipped += 1;
  }
  return { parts, skipped, hidden, failed };
}
