//! Bounded in-process snapshot queue (SBS-1032).
//!
//! The native listener materializes full PNG/HTML/RTF payloads and used to
//! hand them to the async consumer over an unbounded channel. A copy flood
//! then held every pending snapshot in RAM until persist caught up.
//!
//! Policy (drop-oldest, with a retain exception):
//! - At most [`SNAPSHOT_QUEUE_CAPACITY`] events may wait.
//! - Non-oversize queued payloads may hold at most
//!   [`SNAPSHOT_QUEUE_MAX_BYTES`] in total. An accepted oversize capture is
//!   excluded from that sum so a later small event does not evict it, but
//!   follow-ups that themselves fit the budget are still RAM-bounded.
//! - Count overflow evicts the oldest droppable item (even an oversize
//!   resident). Byte overflow evicts the oldest droppable non-oversize item.
//! - [`SizedPayload::retain_on_flood`] items (clipboard `Cleared`) are not
//!   evicted to make room. A 0-byte clear used to be the first byte-budget
//!   victim, which skipped credential forget-on-clear (SBS-1045).
//! - Adjacent retain items that [`SizedPayload::replaces_queued`] accepts
//!   collapse into one slot so an OS clear flood cannot fill the queue.
//! - A single event larger than the byte budget is still accepted after
//!   droppable items are evicted: we never refuse the current clipboard, we
//!   refuse an unbounded backlog. A later oversized event replaces that
//!   capture. Retained clears stay in place.
//! - A 100-copy text burst (the reliability contract) stays under both
//!   budgets, so capture order is preserved under normal load.
//! - Evicted items are returned (not dropped in place) so a bound
//!   self-paste ignore can be released (SBS-1039).
//!
//! Kept free of the Windows-only crate graph so
//! `rustc --test src-tauri/src/clipboard/snapshot_queue.rs` can prove the
//! bound on Linux. Windows CI runs the same tests via `cargo test`.

use std::collections::VecDeque;

/// Enough slots for the 100-copy reliability burst, plus a little headroom
/// for a clear or two that lands in the same window.
pub(crate) const SNAPSHOT_QUEUE_CAPACITY: usize = 128;

/// Hard RAM cap on queued payload bytes. Count-only bounding would still
/// allow 128 large screenshots to pin gigabytes.
pub(crate) const SNAPSHOT_QUEUE_MAX_BYTES: usize = 64 * 1024 * 1024;

pub(crate) trait SizedPayload {
    fn payload_bytes(&self) -> usize;

    /// Stay queued when flood eviction needs a slot or byte-budget room.
    /// `Cleared` is the only production case (SBS-1045).
    fn retain_on_flood(&self) -> bool {
        false
    }

    /// Replace `older` at the back instead of taking another slot.
    /// Adjacent `Cleared` events coalesce so an OS clear flood cannot fill
    /// the queue with retain items (SBS-1045).
    fn replaces_queued(&self, _older: &Self) -> bool {
        false
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EnqueueOutcome<T> {
    Queued,
    QueuedAfterDrop {
        dropped: usize,
        dropped_bytes: usize,
        /// Evicted work, oldest first. Callers that bound ignore-hash to a
        /// snapshot must release that binding here (SBS-1039): process time
        /// never sees these items.
        evicted: Vec<T>,
    },
}

/// Flood eviction can remove the content that separated two retained clears.
/// Collapse any newly adjacent replacements so alternating clear/content
/// traffic cannot grow the retained portion beyond the queue bound.
fn coalesce_adjacent_replacements<T: SizedPayload>(
    queue: &mut VecDeque<T>,
    queued_bytes: &mut usize,
) -> usize {
    let mut index = 0;
    let mut removed_bytes = 0usize;
    while index + 1 < queue.len() {
        if queue[index + 1].replaces_queued(&queue[index]) {
            if let Some(old) = queue.remove(index) {
                let old_bytes = old.payload_bytes();
                *queued_bytes = queued_bytes.saturating_sub(old_bytes);
                removed_bytes = removed_bytes.saturating_add(old_bytes);
            }
        } else {
            index += 1;
        }
    }
    removed_bytes
}

/// Apply the SBS-1032 bound. `capacity` and `max_bytes` are parameters so a
/// test can prove the refusal without allocating the production 64 MiB budget.
pub(crate) fn enqueue<T: SizedPayload>(
    queue: &mut VecDeque<T>,
    queued_bytes: &mut usize,
    item: T,
    capacity: usize,
    max_bytes: usize,
) -> EnqueueOutcome<T> {
    debug_assert!(
        capacity >= 1,
        "SBS-1032: the queue must be able to hold the current capture"
    );

    // Adjacent retain items (repeated Cleared) share one slot so an OS
    // clear flood cannot crowd out the 100-copy burst (SBS-1045).
    if queue
        .back()
        .is_some_and(|older| item.replaces_queued(older))
    {
        if let Some(old) = queue.pop_back() {
            *queued_bytes = queued_bytes.saturating_sub(old.payload_bytes());
        }
    }

    let item_bytes = item.payload_bytes();
    let mut dropped = 0;
    let mut dropped_bytes = 0;
    let mut evicted = Vec::new();

    if item_bytes > max_bytes {
        // Newest oversized capture wins: refuse an unbounded backlog of
        // large payloads, never the current clipboard. Keep retain items
        // so a password-manager clear is not discarded (SBS-1045).
        let mut index = 0;
        while index < queue.len() {
            if queue[index].retain_on_flood() {
                index += 1;
                continue;
            }
            let Some(old) = queue.remove(index) else {
                break;
            };
            let old_bytes = old.payload_bytes();
            *queued_bytes = queued_bytes.saturating_sub(old_bytes);
            dropped += 1;
            dropped_bytes += old_bytes;
            evicted.push(old);
            let _ = coalesce_adjacent_replacements(queue, queued_bytes);
        }
    } else {
        // Counted bytes skip an already-accepted oversize resident so a
        // 0-byte clear or small text copy does not evict it.
        let mut counted_bytes = counted_non_oversize_bytes(queue, max_bytes);
        loop {
            let count_ok = queue.len() < capacity;
            let bytes_ok = counted_bytes.saturating_add(item_bytes) <= max_bytes;
            if count_ok && bytes_ok {
                break;
            }
            if !count_ok {
                // Skip retain items so a fronted Cleared is not the FIFO
                // victim (SBS-1045). If the queue is retain-only, stop:
                // coalescing should have prevented that, and we still
                // accept the current clipboard below.
                let Some(index) = queue.iter().position(|queued| !queued.retain_on_flood()) else {
                    break;
                };
                let Some(old) = queue.remove(index) else {
                    break;
                };
                let old_bytes = old.payload_bytes();
                *queued_bytes = queued_bytes.saturating_sub(old_bytes);
                if old_bytes <= max_bytes {
                    counted_bytes = counted_bytes.saturating_sub(old_bytes);
                }
                dropped += 1;
                dropped_bytes += old_bytes;
                evicted.push(old);
                let coalesced_bytes = coalesce_adjacent_replacements(queue, queued_bytes);
                counted_bytes = counted_bytes.saturating_sub(coalesced_bytes);
            } else if let Some(index) = queue
                .iter()
                .position(|queued| !queued.retain_on_flood() && queued.payload_bytes() <= max_bytes)
            {
                let Some(old) = queue.remove(index) else {
                    break;
                };
                let old_bytes = old.payload_bytes();
                *queued_bytes = queued_bytes.saturating_sub(old_bytes);
                counted_bytes = counted_bytes.saturating_sub(old_bytes);
                dropped += 1;
                dropped_bytes += old_bytes;
                evicted.push(old);
                let coalesced_bytes = coalesce_adjacent_replacements(queue, queued_bytes);
                counted_bytes = counted_bytes.saturating_sub(coalesced_bytes);
            } else {
                break;
            }
        }
    }

    *queued_bytes = queued_bytes.saturating_add(item_bytes);
    queue.push_back(item);

    if dropped == 0 {
        EnqueueOutcome::Queued
    } else {
        EnqueueOutcome::QueuedAfterDrop {
            dropped,
            dropped_bytes,
            evicted,
        }
    }
}

pub(crate) fn dequeue<T: SizedPayload>(
    queue: &mut VecDeque<T>,
    queued_bytes: &mut usize,
) -> Option<T> {
    let item = queue.pop_front()?;
    *queued_bytes = queued_bytes.saturating_sub(item.payload_bytes());
    Some(item)
}

fn counted_non_oversize_bytes<T: SizedPayload>(queue: &VecDeque<T>, max_bytes: usize) -> usize {
    queue
        .iter()
        .map(SizedPayload::payload_bytes)
        .filter(|&bytes| bytes <= max_bytes)
        .fold(0, usize::saturating_add)
}

#[cfg(test)]
mod tests {
    use super::{
        dequeue, enqueue, EnqueueOutcome, SizedPayload, SNAPSHOT_QUEUE_CAPACITY,
        SNAPSHOT_QUEUE_MAX_BYTES,
    };
    use std::collections::VecDeque;

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct FakeSnapshot {
        id: u32,
        bytes: usize,
        retain: bool,
        kind: u8,
    }

    impl SizedPayload for FakeSnapshot {
        fn payload_bytes(&self) -> usize {
            self.bytes
        }

        fn retain_on_flood(&self) -> bool {
            self.retain
        }

        fn replaces_queued(&self, older: &Self) -> bool {
            self.retain && older.retain && self.kind == older.kind
        }
    }

    fn fake(id: u32, bytes: usize) -> FakeSnapshot {
        FakeSnapshot {
            id,
            bytes,
            retain: false,
            kind: 0,
        }
    }

    /// 0-byte retain item: the production `Cleared` shape (SBS-1045).
    fn cleared(id: u32) -> FakeSnapshot {
        FakeSnapshot {
            id,
            bytes: 0,
            retain: true,
            kind: 1,
        }
    }

    fn ids(queue: &VecDeque<FakeSnapshot>) -> Vec<u32> {
        queue.iter().map(|item| item.id).collect()
    }

    fn counted(queue: &VecDeque<FakeSnapshot>, max_bytes: usize) -> usize {
        super::counted_non_oversize_bytes(queue, max_bytes)
    }

    /// Fail-without-fix for SBS-1032: the old unbounded push retains every
    /// payload. 200 × 1 MiB is 200 MiB — over both shipped budgets. If this
    /// assertion ever fails, the leak we are closing has changed shape.
    #[test]
    fn an_unbounded_push_retains_every_flooded_payload() {
        let mut queue = VecDeque::new();
        let mut bytes = 0usize;
        for id in 0..200 {
            let item = fake(id, 1024 * 1024);
            bytes += item.payload_bytes();
            queue.push_back(item);
        }
        assert!(
            queue.len() > SNAPSHOT_QUEUE_CAPACITY,
            "the old channel kept more items than the bound allows"
        );
        assert!(
            bytes > SNAPSHOT_QUEUE_MAX_BYTES,
            "the old channel kept more payload bytes than the RAM budget"
        );
        assert_eq!(queue.len(), 200);
        assert_eq!(bytes, 200 * 1024 * 1024);
    }

    /// Replacing [`enqueue`] with `queue.push_back(item)` makes these
    /// assertions fail — that is the SBS-1032 leak.
    #[test]
    fn a_flood_of_large_payloads_cannot_grow_past_the_budget() {
        let mut queue = VecDeque::new();
        let mut bytes = 0usize;
        for id in 0..200 {
            enqueue(
                &mut queue,
                &mut bytes,
                fake(id, 1024 * 1024),
                SNAPSHOT_QUEUE_CAPACITY,
                SNAPSHOT_QUEUE_MAX_BYTES,
            );
            assert!(queue.len() <= SNAPSHOT_QUEUE_CAPACITY);
            assert!(
                bytes <= SNAPSHOT_QUEUE_MAX_BYTES || queue.len() == 1,
                "queued bytes {bytes} escaped the RAM budget with {} items",
                queue.len()
            );
        }
        // 64 × 1 MiB fills the byte budget before the 128-item cap.
        assert_eq!(queue.len(), SNAPSHOT_QUEUE_MAX_BYTES / (1024 * 1024));
        assert_eq!(bytes, SNAPSHOT_QUEUE_MAX_BYTES);
        assert_eq!(
            queue.front().map(|item| item.id),
            Some(200 - queue.len() as u32)
        );
        assert_eq!(queue.back().map(|item| item.id), Some(199));
    }

    #[test]
    fn a_100_copy_text_burst_is_kept_in_order() {
        let mut queue = VecDeque::new();
        let mut bytes = 0usize;
        for id in 0..100 {
            let outcome = enqueue(
                &mut queue,
                &mut bytes,
                fake(id, 64),
                SNAPSHOT_QUEUE_CAPACITY,
                SNAPSHOT_QUEUE_MAX_BYTES,
            );
            assert_eq!(outcome, EnqueueOutcome::Queued);
        }
        assert_eq!(queue.len(), 100);
        assert_eq!(ids(&queue), (0..100).collect::<Vec<u32>>());
    }

    #[test]
    fn overflowing_the_count_drops_the_oldest() {
        let mut queue = VecDeque::new();
        let mut bytes = 0usize;
        for id in 0..5 {
            enqueue(&mut queue, &mut bytes, fake(id, 1), 3, 1_000);
        }
        assert_eq!(ids(&queue), vec![2, 3, 4]);
        assert_eq!(bytes, 3);
    }

    #[test]
    fn overflowing_the_byte_budget_drops_the_oldest() {
        let mut queue = VecDeque::new();
        let mut bytes = 0usize;
        assert_eq!(
            enqueue(&mut queue, &mut bytes, fake(1, 8), 8, 20),
            EnqueueOutcome::Queued
        );
        assert_eq!(
            enqueue(&mut queue, &mut bytes, fake(2, 8), 8, 20),
            EnqueueOutcome::Queued
        );
        assert_eq!(
            enqueue(&mut queue, &mut bytes, fake(3, 8), 8, 20),
            EnqueueOutcome::QueuedAfterDrop {
                dropped: 1,
                dropped_bytes: 8,
                evicted: vec![fake(1, 8)],
            }
        );
        assert_eq!(ids(&queue), vec![2, 3]);
        assert_eq!(bytes, 16);
    }

    #[test]
    fn a_single_oversize_payload_is_accepted_after_evicting_the_backlog() {
        let mut queue = VecDeque::new();
        let mut bytes = 0usize;
        enqueue(&mut queue, &mut bytes, fake(1, 10), 8, 20);
        enqueue(&mut queue, &mut bytes, fake(2, 10), 8, 20);
        let outcome = enqueue(&mut queue, &mut bytes, fake(3, 50), 8, 20);
        assert_eq!(
            outcome,
            EnqueueOutcome::QueuedAfterDrop {
                dropped: 2,
                dropped_bytes: 20,
                evicted: vec![fake(1, 10), fake(2, 10)],
            }
        );
        assert_eq!(ids(&queue), vec![3]);
        assert_eq!(bytes, 50);
    }

    /// Fail-without-fix: after an over-budget snapshot is accepted into an
    /// empty queue, the next 0-byte/small event must not pop it just because
    /// `queued_bytes` is already over the budget. A later oversized capture
    /// may still replace it.
    #[test]
    fn a_small_follow_up_does_not_evict_an_accepted_oversize_snapshot() {
        let mut queue = VecDeque::new();
        let mut bytes = 0usize;
        assert_eq!(
            enqueue(&mut queue, &mut bytes, fake(1, 50), 8, 20),
            EnqueueOutcome::Queued
        );
        assert_eq!(ids(&queue), vec![1]);
        assert_eq!(bytes, 50);

        assert_eq!(
            enqueue(&mut queue, &mut bytes, fake(2, 0), 8, 20),
            EnqueueOutcome::Queued
        );
        assert_eq!(ids(&queue), vec![1, 2]);
        assert_eq!(queue.front().map(|item| item.id), Some(1));
        assert_eq!(bytes, 50);

        assert_eq!(
            enqueue(&mut queue, &mut bytes, fake(3, 1), 8, 20),
            EnqueueOutcome::Queued
        );
        assert_eq!(ids(&queue), vec![1, 2, 3]);
        assert_eq!(bytes, 51);

        let outcome = enqueue(&mut queue, &mut bytes, fake(4, 60), 8, 20);
        assert_eq!(
            outcome,
            EnqueueOutcome::QueuedAfterDrop {
                dropped: 3,
                dropped_bytes: 51,
                evicted: vec![fake(1, 50), fake(2, 0), fake(3, 1)],
            }
        );
        assert_eq!(ids(&queue), vec![4]);
        assert_eq!(bytes, 60);
    }

    #[test]
    fn small_follow_ups_after_oversize_still_respect_the_count_cap() {
        let mut queue = VecDeque::new();
        let mut bytes = 0usize;
        enqueue(&mut queue, &mut bytes, fake(0, 50), 3, 20);
        enqueue(&mut queue, &mut bytes, fake(1, 1), 3, 20);
        enqueue(&mut queue, &mut bytes, fake(2, 1), 3, 20);
        assert_eq!(ids(&queue), vec![0, 1, 2]);
        let outcome = enqueue(&mut queue, &mut bytes, fake(3, 1), 3, 20);
        assert_eq!(
            outcome,
            EnqueueOutcome::QueuedAfterDrop {
                dropped: 1,
                dropped_bytes: 50,
                evicted: vec![fake(0, 50)],
            }
        );
        assert_eq!(ids(&queue), vec![1, 2, 3]);
        assert_eq!(queue.len(), 3);
        assert_eq!(bytes, 3);
    }

    /// Fail-without-fix: after an oversize snapshot is accepted, follow-ups
    /// that themselves fit the budget must still be byte-evicted. The oversize
    /// resident is excluded from the counted sum and must stay at the front.
    #[test]
    fn follow_ups_after_oversize_are_still_byte_bounded() {
        let mut queue = VecDeque::new();
        let mut bytes = 0usize;
        enqueue(&mut queue, &mut bytes, fake(1, 50), 8, 20);
        assert_eq!(
            enqueue(&mut queue, &mut bytes, fake(2, 8), 8, 20),
            EnqueueOutcome::Queued
        );
        assert_eq!(
            enqueue(&mut queue, &mut bytes, fake(3, 8), 8, 20),
            EnqueueOutcome::Queued
        );
        let outcome = enqueue(&mut queue, &mut bytes, fake(4, 8), 8, 20);
        assert_eq!(
            outcome,
            EnqueueOutcome::QueuedAfterDrop {
                dropped: 1,
                dropped_bytes: 8,
                evicted: vec![fake(2, 8)],
            }
        );
        assert_eq!(ids(&queue), vec![1, 3, 4]);
        assert_eq!(queue.front().map(|item| item.id), Some(1));
        assert_eq!(counted(&queue, 20), 16);
        assert!(counted(&queue, 20) <= 20);
        assert_eq!(bytes, 66);
    }

    #[test]
    fn a_flood_of_under_budget_follow_ups_after_oversize_cannot_grow_past_the_budget() {
        let mut queue = VecDeque::new();
        let mut bytes = 0usize;
        let oversize = SNAPSHOT_QUEUE_MAX_BYTES + 1;
        enqueue(
            &mut queue,
            &mut bytes,
            fake(0, oversize),
            SNAPSHOT_QUEUE_CAPACITY,
            SNAPSHOT_QUEUE_MAX_BYTES,
        );
        for id in 1..=200u32 {
            enqueue(
                &mut queue,
                &mut bytes,
                fake(id, 1024 * 1024),
                SNAPSHOT_QUEUE_CAPACITY,
                SNAPSHOT_QUEUE_MAX_BYTES,
            );
            assert!(queue.len() <= SNAPSHOT_QUEUE_CAPACITY);
            assert!(counted(&queue, SNAPSHOT_QUEUE_MAX_BYTES) <= SNAPSHOT_QUEUE_MAX_BYTES);
            assert_eq!(queue.front().map(|item| item.id), Some(0));
        }
        assert_eq!(
            counted(&queue, SNAPSHOT_QUEUE_MAX_BYTES),
            SNAPSHOT_QUEUE_MAX_BYTES
        );
        assert_eq!(queue.len(), 1 + SNAPSHOT_QUEUE_MAX_BYTES / (1024 * 1024));
        assert_eq!(bytes, oversize + SNAPSHOT_QUEUE_MAX_BYTES);
    }

    #[test]
    fn dequeue_releases_the_byte_budget() {
        let mut queue = VecDeque::new();
        let mut bytes = 0usize;
        enqueue(&mut queue, &mut bytes, fake(1, 10), 4, 40);
        enqueue(&mut queue, &mut bytes, fake(2, 10), 4, 40);
        assert_eq!(dequeue(&mut queue, &mut bytes).map(|item| item.id), Some(1));
        assert_eq!(bytes, 10);
        assert_eq!(dequeue(&mut queue, &mut bytes).map(|item| item.id), Some(2));
        assert_eq!(bytes, 0);
        assert!(dequeue(&mut queue, &mut bytes).is_none());
    }

    /// Fail-without-fix for SBS-1039: counting drops is not enough. The
    /// evicted items themselves must come back so a bound ignore-hash can
    /// be released. Replacing the return with counts-only makes this fail.
    #[test]
    fn enqueue_returns_the_evicted_items() {
        let mut queue = VecDeque::new();
        let mut bytes = 0usize;
        enqueue(&mut queue, &mut bytes, fake(1, 1), 2, 100);
        enqueue(&mut queue, &mut bytes, fake(2, 1), 2, 100);
        let outcome = enqueue(&mut queue, &mut bytes, fake(3, 1), 2, 100);
        match outcome {
            EnqueueOutcome::QueuedAfterDrop { evicted, .. } => {
                assert_eq!(evicted, vec![fake(1, 1)]);
            }
            EnqueueOutcome::Queued => {
                panic!("fail-without-fix: enqueue discarded the evicted item")
            }
        }
    }

    #[test]
    fn the_shipped_capacity_covers_the_100_copy_reliability_burst() {
        // Both sides are constants, so this is a compile-time fact.
        // `const {}` satisfies clippy::assertions_on_constants.
        const {
            assert!(
                SNAPSHOT_QUEUE_CAPACITY >= 100,
                "the reliability contract captures a 100-copy burst; the queue must hold it"
            )
        };
    }

    /// Fail-without-fix for SBS-1045: count overflow used `pop_front`, so a
    /// Cleared sitting at the head was discarded and forget-on-clear never
    /// ran. Evict the oldest droppable item instead.
    #[test]
    fn a_retained_clear_survives_count_overflow() {
        let mut queue = VecDeque::new();
        let mut bytes = 0usize;
        enqueue(&mut queue, &mut bytes, cleared(0), 3, 1_000);
        enqueue(&mut queue, &mut bytes, fake(1, 1), 3, 1_000);
        enqueue(&mut queue, &mut bytes, fake(2, 1), 3, 1_000);
        let outcome = enqueue(&mut queue, &mut bytes, fake(3, 1), 3, 1_000);
        assert_eq!(
            outcome,
            EnqueueOutcome::QueuedAfterDrop {
                dropped: 1,
                dropped_bytes: 1,
                evicted: vec![fake(1, 1)],
            }
        );
        assert_eq!(ids(&queue), vec![0, 2, 3]);
        assert!(queue.front().is_some_and(|item| item.retain));
        assert_eq!(bytes, 2);
    }

    /// Fail-without-fix for SBS-1045: byte overflow evicted the oldest
    /// non-oversize item, and a 0-byte Cleared always qualified first.
    #[test]
    fn a_retained_clear_survives_byte_overflow() {
        let mut queue = VecDeque::new();
        let mut bytes = 0usize;
        enqueue(&mut queue, &mut bytes, cleared(0), 8, 20);
        enqueue(&mut queue, &mut bytes, fake(1, 8), 8, 20);
        enqueue(&mut queue, &mut bytes, fake(2, 8), 8, 20);
        assert_eq!(ids(&queue), vec![0, 1, 2]);
        let outcome = enqueue(&mut queue, &mut bytes, fake(3, 8), 8, 20);
        assert_eq!(
            outcome,
            EnqueueOutcome::QueuedAfterDrop {
                dropped: 1,
                dropped_bytes: 8,
                evicted: vec![fake(1, 8)],
            }
        );
        assert_eq!(ids(&queue), vec![0, 2, 3]);
        assert!(queue.iter().any(|item| item.retain && item.id == 0));
        assert_eq!(bytes, 16);
    }

    /// An oversized follow-up may replace the capture backlog, not the
    /// pending clear that still has to drive forget-on-clear.
    #[test]
    fn a_retained_clear_survives_an_oversize_replacement() {
        let mut queue = VecDeque::new();
        let mut bytes = 0usize;
        enqueue(&mut queue, &mut bytes, fake(1, 10), 8, 20);
        enqueue(&mut queue, &mut bytes, cleared(2), 8, 20);
        enqueue(&mut queue, &mut bytes, fake(3, 10), 8, 20);
        let outcome = enqueue(&mut queue, &mut bytes, fake(4, 50), 8, 20);
        assert_eq!(
            outcome,
            EnqueueOutcome::QueuedAfterDrop {
                dropped: 2,
                dropped_bytes: 20,
                evicted: vec![fake(1, 10), fake(3, 10)],
            }
        );
        assert_eq!(ids(&queue), vec![2, 4]);
        assert!(queue.front().is_some_and(|item| item.retain));
        assert_eq!(bytes, 50);
    }

    /// Fail-without-fix: an OS clear flood used to occupy every slot with
    /// 0-byte Cleared events. Adjacent clears collapse to the newest.
    #[test]
    fn adjacent_retained_clears_coalesce_instead_of_filling_the_queue() {
        let mut queue = VecDeque::new();
        let mut bytes = 0usize;
        for id in 0..200 {
            let outcome = enqueue(&mut queue, &mut bytes, cleared(id), 8, 20);
            assert_eq!(outcome, EnqueueOutcome::Queued);
            assert_eq!(queue.len(), 1);
            assert_eq!(bytes, 0);
        }
        assert_eq!(ids(&queue), vec![199]);
        assert!(queue.front().is_some_and(|item| item.retain));
    }

    /// Evicting the content between retained clears makes those clears
    /// equivalent. They must then coalesce or alternating traffic can grow
    /// the retained queue past its configured capacity.
    #[test]
    fn alternating_clear_and_content_flood_stays_count_bounded() {
        let mut queue = VecDeque::new();
        let mut bytes = 0usize;
        for id in 0..500 {
            enqueue(&mut queue, &mut bytes, cleared(id * 2), 8, 20);
            enqueue(&mut queue, &mut bytes, fake(id * 2 + 1, 8), 8, 20);
            assert!(queue.len() <= 8, "queue grew to {} items", queue.len());
            assert!(bytes <= 20, "queue grew to {bytes} bytes");
        }
    }

    #[test]
    fn a_retained_clear_does_not_steal_the_100_copy_burst() {
        let mut queue = VecDeque::new();
        let mut bytes = 0usize;
        enqueue(
            &mut queue,
            &mut bytes,
            cleared(0),
            SNAPSHOT_QUEUE_CAPACITY,
            SNAPSHOT_QUEUE_MAX_BYTES,
        );
        for id in 1..=100 {
            let outcome = enqueue(
                &mut queue,
                &mut bytes,
                fake(id, 64),
                SNAPSHOT_QUEUE_CAPACITY,
                SNAPSHOT_QUEUE_MAX_BYTES,
            );
            assert_eq!(outcome, EnqueueOutcome::Queued);
        }
        assert_eq!(queue.len(), 101);
        assert!(queue
            .front()
            .is_some_and(|item| item.retain && item.id == 0));
        assert_eq!(
            ids(&queue),
            std::iter::once(0).chain(1..=100).collect::<Vec<u32>>()
        );
    }

    #[test]
    fn a_clear_between_content_is_not_moved_behind_later_copies() {
        let mut queue = VecDeque::new();
        let mut bytes = 0usize;
        enqueue(&mut queue, &mut bytes, fake(1, 1), 3, 1_000);
        enqueue(&mut queue, &mut bytes, cleared(2), 3, 1_000);
        enqueue(&mut queue, &mut bytes, fake(3, 1), 3, 1_000);
        enqueue(&mut queue, &mut bytes, fake(4, 1), 3, 1_000);
        // Evict the oldest droppable (id 1), leave Cleared ahead of id 3.
        // Rotating Cleared to the back would let a later copy overwrite
        // LAST_ACCEPTED_CAPTURE before forget-on-clear runs.
        assert_eq!(ids(&queue), vec![2, 3, 4]);
        assert!(queue
            .front()
            .is_some_and(|item| item.retain && item.id == 2));
    }
}
