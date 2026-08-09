import { useCallback, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { toast } from 'sonner';
import { ClipboardItem } from '../types';

/**
 * Session-scoped reveal for hidden clips (SOU-586).
 *
 * A hidden clip's row arrives with no content at all, so revealing it means
 * fetching the payload rather than un-blanking something already in hand. That
 * fetched copy lives only here, in component state: it is never written back to
 * the list, so it disappears when the window closes and the persisted hidden
 * flag is untouched.
 */
export function useRevealedClips() {
  const [revealed, setRevealed] = useState<Map<string, ClipboardItem>>(new Map());
  // Intent is tracked in a ref, not in `revealed`, because a fetch spans
  // renders: two clicks before the response lands would both read the same
  // stale map, both start a fetch, and the second — meant to re-hide — would
  // instead reveal. The ref updates synchronously, so the second click sees the
  // first and cancels it.
  const pending = useRef<Set<string>>(new Set());

  const toggleReveal = useCallback(async (clip: ClipboardItem) => {
    const hide = () => {
      pending.current.delete(clip.id);
      setRevealed((current) => {
        if (!current.has(clip.id)) return current;
        const next = new Map(current);
        next.delete(clip.id);
        return next;
      });
    };

    if (pending.current.has(clip.id)) {
      // Already revealed, or a reveal is in flight. Either way this click means
      // "hide"; the in-flight response checks the set again before applying.
      hide();
      return;
    }

    pending.current.add(clip.id);
    try {
      const details = await invoke<{ content: string }>('get_clip_details', { id: clip.id });
      // Cancelled while the fetch was out.
      if (!pending.current.has(clip.id)) return;
      setRevealed((current) => {
        const next = new Map(current);
        // Keep the row's own metadata; only the payload was withheld.
        next.set(clip.id, { ...clip, content: details.content, preview: details.content });
        return next;
      });
    } catch (error) {
      console.error('Failed to reveal clip:', error);
      pending.current.delete(clip.id);
      // The eye button has no other failure state: without this the row simply
      // stays blank and the click looks ignored. The persisted hide toggle
      // already toasts on failure, so this matches it.
      toast.error('Could not reveal this clip');
    }
  }, []);

  /**
   * Forget one clip's reveal, including an in-flight one.
   *
   * Toggling the hidden flag invalidates that clip's reveal and nothing else,
   * so it must not disturb the rest: clearing the whole map would drop a
   * different clip's pending entry, and its fetch would then land to a closed
   * door — the user clicked reveal and silently got nothing.
   */
  const forgetRevealed = useCallback((clipId: string) => {
    pending.current.delete(clipId);
    setRevealed((current) => {
      if (!current.has(clipId)) return current;
      const next = new Map(current);
      next.delete(clipId);
      return next;
    });
  }, []);

  /** Forget every reveal — used when the surface closes or the list reloads. */
  const clearRevealed = useCallback(() => {
    pending.current.clear();
    setRevealed(new Map());
  }, []);

  return { revealed, toggleReveal, forgetRevealed, clearRevealed };
}
