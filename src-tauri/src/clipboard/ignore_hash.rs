//! Self-paste ignore-hash lifetime (SBS-1022).
//!
//! Isolated so enqueue binding vs wall-clock TTL can be tested without the
//! Windows crate. A 5s marker that expires before a queued echo is processed
//! used to persist that echo and bump `created_at`/`source_app`. The work item
//! now carries the hash that was live at enqueue.

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

/// Whether a self-write marker still applies to `hash`.
///
/// A marker that has outlived `ttl` is treated as absent: the write it
/// described was never observed, and honouring it would drop a real copy.
/// Queued work that already bound this hash uses
/// [`should_ignore_queued_self_paste`] instead of this clock alone.
pub(crate) fn ignore_marker_applies(
    marker: Option<&(String, Instant)>,
    hash: &str,
    now: Instant,
    ttl: Duration,
) -> bool {
    marker.is_some_and(|(marked, marked_at)| {
        marked == hash
            && now
                .checked_duration_since(*marked_at)
                .is_some_and(|elapsed| elapsed <= ttl)
    })
}

/// Copy a still-live ignore hash onto a snapshot as it enters the queue.
///
/// The wall-clock TTL decides only whether the marker is live *at enqueue*.
/// An already-expired marker must not ride along and swallow a later real
/// copy. A live one travels with the work item so process time can honour it
/// after the TTL (SBS-1022).
pub(crate) fn bind_ignore_hash_for_queued_work(
    marker: Option<&(String, Instant)>,
    now: Instant,
    ttl: Duration,
) -> Option<String> {
    marker.and_then(|(hash, marked_at)| {
        now.checked_duration_since(*marked_at)
            .is_some_and(|elapsed| elapsed <= ttl)
            .then(|| hash.clone())
    })
}

/// Whether this queued snapshot is Cubby's own write and must not be persisted.
///
/// The work item's bound hash stays effective for the snapshot's lifetime,
/// even when the live marker has outlived [`IGNORE_HASH_TTL`]. The live-marker
/// check still covers a write whose snapshot was queued in the same window
/// the paste path set the hash.
pub(crate) fn should_ignore_queued_self_paste(
    queued_ignore_hash: Option<&str>,
    live_marker: Option<&(String, Instant)>,
    clip_hash: &str,
    now: Instant,
    ttl: Duration,
) -> bool {
    queued_ignore_hash == Some(clip_hash) || ignore_marker_applies(live_marker, clip_hash, now, ttl)
}

#[cfg(test)]
mod tests {
    use super::{
        bind_ignore_hash_for_queued_work, ignore_marker_applies, should_ignore_queued_self_paste,
        IGNORE_HASH_TTL,
    };
    use std::time::{Duration, Instant};

    #[test]
    fn ignore_marker_applies_to_a_fresh_matching_write() {
        let now = Instant::now();
        let marker = ("hash-a".to_string(), now);
        assert!(ignore_marker_applies(
            Some(&marker),
            "hash-a",
            now + Duration::from_millis(50),
            IGNORE_HASH_TTL
        ));
    }

    #[test]
    fn ignore_marker_never_applies_to_other_content() {
        let now = Instant::now();
        let marker = ("hash-a".to_string(), now);
        assert!(!ignore_marker_applies(
            Some(&marker),
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
        let marker = ("hash-a".to_string(), now);
        assert!(!ignore_marker_applies(
            Some(&marker),
            "hash-a",
            now + IGNORE_HASH_TTL + Duration::from_millis(1),
            IGNORE_HASH_TTL
        ));
    }

    #[test]
    fn bind_ignore_hash_attaches_a_live_marker_to_queued_work() {
        let now = Instant::now();
        let marker = ("hash-a".to_string(), now);
        assert_eq!(
            bind_ignore_hash_for_queued_work(Some(&marker), now, IGNORE_HASH_TTL).as_deref(),
            Some("hash-a")
        );
    }

    #[test]
    fn bind_ignore_hash_does_not_attach_an_expired_marker() {
        // A marker that already expired before enqueue must not ride along.
        // That is a later real copy of the same content, not our write.
        let now = Instant::now();
        let marker = ("hash-a".to_string(), now);
        let later = now + IGNORE_HASH_TTL + Duration::from_millis(1);
        assert_eq!(
            bind_ignore_hash_for_queued_work(Some(&marker), later, IGNORE_HASH_TTL),
            None
        );
        assert!(!should_ignore_queued_self_paste(
            None,
            Some(&marker),
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
        let marker = ("hash-a".to_string(), now);
        let queued = bind_ignore_hash_for_queued_work(Some(&marker), now, IGNORE_HASH_TTL);
        let later = now + IGNORE_HASH_TTL + Duration::from_millis(1);

        assert_eq!(queued.as_deref(), Some("hash-a"));
        assert!(
            !ignore_marker_applies(Some(&marker), "hash-a", later, IGNORE_HASH_TTL),
            "fail-without-fix: a wall-clock TTL treats this queued echo as a new capture"
        );
        assert!(should_ignore_queued_self_paste(
            queued.as_deref(),
            Some(&marker),
            "hash-a",
            later,
            IGNORE_HASH_TTL
        ));
    }

    #[test]
    fn queued_ignore_hash_does_not_ignore_other_content() {
        let now = Instant::now();
        let marker = ("hash-a".to_string(), now);
        let queued = bind_ignore_hash_for_queued_work(Some(&marker), now, IGNORE_HASH_TTL);
        assert!(!should_ignore_queued_self_paste(
            queued.as_deref(),
            Some(&marker),
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
        let first = ("hash-a".to_string(), now);
        let queued = bind_ignore_hash_for_queued_work(Some(&first), now, IGNORE_HASH_TTL);
        let replaced = ("hash-b".to_string(), now);
        let later = now + IGNORE_HASH_TTL + Duration::from_millis(1);

        assert!(should_ignore_queued_self_paste(
            queued.as_deref(),
            Some(&replaced),
            "hash-a",
            later,
            IGNORE_HASH_TTL
        ));
        assert!(!should_ignore_queued_self_paste(
            queued.as_deref(),
            Some(&replaced),
            "hash-b",
            later,
            IGNORE_HASH_TTL
        ));
    }

    #[test]
    fn live_marker_still_covers_a_snapshot_queued_without_a_binding() {
        let now = Instant::now();
        let marker = ("hash-a".to_string(), now);
        assert!(should_ignore_queued_self_paste(
            None,
            Some(&marker),
            "hash-a",
            now + Duration::from_millis(50),
            IGNORE_HASH_TTL
        ));
    }
}
