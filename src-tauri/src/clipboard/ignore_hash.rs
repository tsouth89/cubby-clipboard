//! Self-paste ignore-hash lifetime (SBS-1022).
//!
//! Isolated so enqueue binding vs wall-clock TTL can be tested without the
//! Windows crate. A 5s marker that expires before a queued echo is processed
//! used to persist that echo and bump `created_at`/`source_app`. The work item
//! now carries the hash (and marker generation) that was live at enqueue.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// How long a self-write marker stays valid *until a snapshot is queued*.
/// The capture of our own write normally arrives within milliseconds, so this
/// is generous. It exists because an unbounded marker is never cleaned up when
/// that capture never arrives at all -- the read lost every race, or a remote
/// client rewrote the clipboard before we could read it -- and a stale marker
/// silently swallows the next legitimate copy of the same content.
///
/// Once a snapshot is queued while the marker is still live, that work item
/// carries the hash for its own lifetime. A backed-up consumer must not treat
/// the echo as a new capture just because this TTL elapsed first (SBS-1022).
pub(crate) const IGNORE_HASH_TTL: Duration = Duration::from_secs(5);

static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

/// Live self-write marker. `generation` identifies this set, so a delayed
/// snapshot cannot consume a later paste of the same content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct IgnoreMarker {
    pub hash: String,
    pub marked_at: Instant,
    pub generation: u64,
}

impl IgnoreMarker {
    pub(crate) fn new(hash: String) -> Self {
        Self {
            hash,
            marked_at: Instant::now(),
            generation: NEXT_GENERATION.fetch_add(1, Ordering::Relaxed),
        }
    }
}

/// Ignore identity copied onto a snapshot at enqueue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct QueuedIgnore {
    pub hash: String,
    pub generation: u64,
}

/// Whether a self-write marker still applies to `hash`.
///
/// A marker that has outlived `ttl` is treated as absent: the write it
/// described was never observed, and honouring it would drop a real copy.
/// Queued work that already bound this hash uses
/// [`should_ignore_queued_self_paste`] instead of this clock alone.
pub(crate) fn ignore_marker_applies(
    marker: Option<&IgnoreMarker>,
    hash: &str,
    now: Instant,
    ttl: Duration,
) -> bool {
    marker.is_some_and(|marked| {
        marked.hash == hash
            && now
                .checked_duration_since(marked.marked_at)
                .is_some_and(|elapsed| elapsed <= ttl)
    })
}

/// Copy a still-live ignore marker onto a snapshot as it enters the queue.
///
/// The wall-clock TTL decides only whether the marker is live *at enqueue*.
/// An already-expired marker must not ride along and swallow a later real
/// copy. A live one travels with the work item so process time can honour it
/// after the TTL (SBS-1022).
pub(crate) fn bind_ignore_hash_for_queued_work(
    marker: Option<&IgnoreMarker>,
    now: Instant,
    ttl: Duration,
) -> Option<QueuedIgnore> {
    marker.and_then(|marked| {
        now.checked_duration_since(marked.marked_at)
            .is_some_and(|elapsed| elapsed <= ttl)
            .then(|| QueuedIgnore {
                hash: marked.hash.clone(),
                generation: marked.generation,
            })
    })
}

/// Whether this queued snapshot is Cubby's own write and must not be persisted.
///
/// The work item's bound hash stays effective for the snapshot's lifetime,
/// even when the live marker has outlived [`IGNORE_HASH_TTL`]. The live-marker
/// check still covers a write whose snapshot was queued in the same window
/// the paste path set the hash.
pub(crate) fn should_ignore_queued_self_paste(
    queued: Option<&QueuedIgnore>,
    live_marker: Option<&IgnoreMarker>,
    clip_hash: &str,
    now: Instant,
    ttl: Duration,
) -> bool {
    queued.is_some_and(|bound| bound.hash == clip_hash)
        || ignore_marker_applies(live_marker, clip_hash, now, ttl)
}

/// Drop the live marker only if this work item owns it, or we ignored via the
/// live TTL path rather than the queued binding.
///
/// Consuming by hash alone would wipe a newer paste of the same content while
/// a delayed echo is still draining. Requiring `queued` to be `None` would
/// leave a live marker after an unrelated binding was ignored via TTL, so a
/// later real copy of that content could be swallowed until the clock expires.
pub(crate) fn consume_ignore_marker_after_self_paste(
    live: &mut Option<IgnoreMarker>,
    queued: Option<&QueuedIgnore>,
    clip_hash: &str,
    now: Instant,
    ttl: Duration,
) {
    let Some(marker) = live.as_ref() else {
        return;
    };
    if marker.hash != clip_hash {
        return;
    }
    let ignored_via_queued = queued.is_some_and(|bound| bound.hash == clip_hash);
    let owns_this_marker = queued.is_some_and(|bound| bound.generation == marker.generation);
    let ignored_via_live = ignore_marker_applies(live.as_ref(), clip_hash, now, ttl);
    if owns_this_marker || (ignored_via_live && !ignored_via_queued) {
        live.take();
    }
}

#[cfg(test)]
mod tests {
    use super::{
        bind_ignore_hash_for_queued_work, consume_ignore_marker_after_self_paste,
        ignore_marker_applies, should_ignore_queued_self_paste, IgnoreMarker, QueuedIgnore,
        IGNORE_HASH_TTL,
    };
    use std::time::{Duration, Instant};

    fn marker(hash: &str, marked_at: Instant, generation: u64) -> IgnoreMarker {
        IgnoreMarker {
            hash: hash.to_string(),
            marked_at,
            generation,
        }
    }

    #[test]
    fn new_markers_get_distinct_generations() {
        let first = IgnoreMarker::new("hash-a".to_string());
        let second = IgnoreMarker::new("hash-a".to_string());
        assert_ne!(first.generation, second.generation);
        assert_eq!(first.hash, second.hash);
    }

    #[test]
    fn ignore_marker_applies_to_a_fresh_matching_write() {
        let now = Instant::now();
        let marked = marker("hash-a", now, 1);
        assert!(ignore_marker_applies(
            Some(&marked),
            "hash-a",
            now + Duration::from_millis(50),
            IGNORE_HASH_TTL
        ));
    }

    #[test]
    fn ignore_marker_never_applies_to_other_content() {
        let now = Instant::now();
        let marked = marker("hash-a", now, 1);
        assert!(!ignore_marker_applies(
            Some(&marked),
            "hash-b",
            now,
            IGNORE_HASH_TTL
        ));
        assert!(!ignore_marker_applies(None, "hash-a", now, IGNORE_HASH_TTL));
    }

    #[test]
    fn a_stale_marker_stops_swallowing_real_copies() {
        // The self-write was never observed (contended read, or a remote client
        // rewrote the clipboard first). Copying that same content later is a
        // genuine copy and must still be captured.
        let now = Instant::now();
        let marked = marker("hash-a", now, 1);
        assert!(!ignore_marker_applies(
            Some(&marked),
            "hash-a",
            now + IGNORE_HASH_TTL + Duration::from_millis(1),
            IGNORE_HASH_TTL
        ));
    }

    #[test]
    fn bind_ignore_hash_attaches_a_live_marker_to_queued_work() {
        let now = Instant::now();
        let marked = marker("hash-a", now, 7);
        assert_eq!(
            bind_ignore_hash_for_queued_work(Some(&marked), now, IGNORE_HASH_TTL),
            Some(QueuedIgnore {
                hash: "hash-a".to_string(),
                generation: 7,
            })
        );
    }

    #[test]
    fn bind_ignore_hash_does_not_attach_an_expired_marker() {
        // A marker that already expired before enqueue must not ride along.
        // That is a later real copy of the same content, not our write.
        let now = Instant::now();
        let marked = marker("hash-a", now, 1);
        let later = now + IGNORE_HASH_TTL + Duration::from_millis(1);
        assert_eq!(
            bind_ignore_hash_for_queued_work(Some(&marked), later, IGNORE_HASH_TTL),
            None
        );
        assert!(!should_ignore_queued_self_paste(
            None,
            Some(&marked),
            "hash-a",
            later,
            IGNORE_HASH_TTL
        ));
    }

    #[test]
    fn queued_self_paste_survives_an_expired_ttl() {
        // SBS-1022: the snapshot was queued while the marker was live, then
        // sat behind other clipboard work until the 5s TTL elapsed. The old
        // process-time clock (`ignore_marker_applies` alone) would persist
        // the echo and bump created_at/source_app. The work item keeps the
        // hash, so the echo is still ignored.
        let now = Instant::now();
        let marked = marker("hash-a", now, 1);
        let queued = bind_ignore_hash_for_queued_work(Some(&marked), now, IGNORE_HASH_TTL);
        let later = now + IGNORE_HASH_TTL + Duration::from_millis(1);

        assert_eq!(
            queued.as_ref().map(|bound| bound.hash.as_str()),
            Some("hash-a")
        );
        assert!(
            !ignore_marker_applies(Some(&marked), "hash-a", later, IGNORE_HASH_TTL),
            "fail-without-fix: a wall-clock TTL treats this queued echo as a new capture"
        );
        assert!(should_ignore_queued_self_paste(
            queued.as_ref(),
            Some(&marked),
            "hash-a",
            later,
            IGNORE_HASH_TTL
        ));
    }

    #[test]
    fn queued_ignore_hash_does_not_ignore_other_content() {
        let now = Instant::now();
        let marked = marker("hash-a", now, 1);
        let queued = bind_ignore_hash_for_queued_work(Some(&marked), now, IGNORE_HASH_TTL);
        assert!(!should_ignore_queued_self_paste(
            queued.as_ref(),
            Some(&marked),
            "hash-b",
            now,
            IGNORE_HASH_TTL
        ));
    }

    #[test]
    fn queued_self_paste_survives_the_live_marker_being_replaced() {
        // Two pastes in flight: the first work item still carries hash-a
        // after the live marker moved on to hash-b.
        let now = Instant::now();
        let first = marker("hash-a", now, 1);
        let queued = bind_ignore_hash_for_queued_work(Some(&first), now, IGNORE_HASH_TTL);
        let replaced = marker("hash-b", now, 2);
        let later = now + IGNORE_HASH_TTL + Duration::from_millis(1);

        assert!(should_ignore_queued_self_paste(
            queued.as_ref(),
            Some(&replaced),
            "hash-a",
            later,
            IGNORE_HASH_TTL
        ));
        assert!(!should_ignore_queued_self_paste(
            queued.as_ref(),
            Some(&replaced),
            "hash-b",
            later,
            IGNORE_HASH_TTL
        ));
    }

    #[test]
    fn live_marker_still_covers_a_snapshot_queued_without_a_binding() {
        let now = Instant::now();
        let marked = marker("hash-a", now, 1);
        assert!(should_ignore_queued_self_paste(
            None,
            Some(&marked),
            "hash-a",
            now + Duration::from_millis(50),
            IGNORE_HASH_TTL
        ));
    }

    #[test]
    fn consuming_a_delayed_echo_does_not_clear_a_newer_same_hash_marker() {
        // Paste A, queue its echo, then paste A again. The delayed first echo
        // must not consume the refreshed marker or the second write is
        // persisted as a new capture.
        let now = Instant::now();
        let first = marker("hash-a", now, 1);
        let queued = bind_ignore_hash_for_queued_work(Some(&first), now, IGNORE_HASH_TTL);
        let refreshed_at = now + Duration::from_secs(1);
        let refreshed = marker("hash-a", refreshed_at, 2);
        let later = now + IGNORE_HASH_TTL + Duration::from_millis(1);

        assert!(should_ignore_queued_self_paste(
            queued.as_ref(),
            Some(&refreshed),
            "hash-a",
            later,
            IGNORE_HASH_TTL
        ));

        let mut live = Some(refreshed.clone());
        consume_ignore_marker_after_self_paste(
            &mut live,
            queued.as_ref(),
            "hash-a",
            later,
            IGNORE_HASH_TTL,
        );
        assert_eq!(
            live.as_ref().map(|marked| marked.generation),
            Some(2),
            "fail-without-fix: consume-by-hash wipes the newer paste's marker"
        );
        assert!(should_ignore_queued_self_paste(
            None,
            live.as_ref(),
            "hash-a",
            refreshed_at + Duration::from_millis(50),
            IGNORE_HASH_TTL
        ));
    }

    #[test]
    fn consume_clears_the_owned_marker_even_after_ttl() {
        let now = Instant::now();
        let marked = marker("hash-a", now, 1);
        let queued = bind_ignore_hash_for_queued_work(Some(&marked), now, IGNORE_HASH_TTL);
        let mut live = Some(marked);
        consume_ignore_marker_after_self_paste(
            &mut live,
            queued.as_ref(),
            "hash-a",
            now + IGNORE_HASH_TTL + Duration::from_millis(1),
            IGNORE_HASH_TTL,
        );
        assert!(live.is_none());
    }

    #[test]
    fn consume_via_live_ttl_clears_an_unbound_marker() {
        let now = Instant::now();
        let mut live = Some(marker("hash-a", now, 1));
        consume_ignore_marker_after_self_paste(
            &mut live,
            None,
            "hash-a",
            now + Duration::from_millis(50),
            IGNORE_HASH_TTL,
        );
        assert!(live.is_none());
    }

    #[test]
    fn live_ttl_ignore_consumes_even_when_queued_binding_is_unrelated() {
        // Snapshot bound to paste A, content is B, live marker is B. We ignore
        // via the live TTL arm. Leaving B in place would keep binding later
        // real copies of B until the clock expires.
        let now = Instant::now();
        let queued_other = QueuedIgnore {
            hash: "hash-a".to_string(),
            generation: 1,
        };
        let live_b = marker("hash-b", now, 2);
        assert!(should_ignore_queued_self_paste(
            Some(&queued_other),
            Some(&live_b),
            "hash-b",
            now + Duration::from_millis(50),
            IGNORE_HASH_TTL
        ));

        let mut live = Some(live_b);
        consume_ignore_marker_after_self_paste(
            &mut live,
            Some(&queued_other),
            "hash-b",
            now + Duration::from_millis(50),
            IGNORE_HASH_TTL,
        );
        assert!(
            live.is_none(),
            "fail-without-fix: leftover live marker swallows later real copies"
        );
    }
}
