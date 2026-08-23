//! Bounded in-process snapshot queue (SBS-1032).
//!
//! The native listener materializes full PNG/HTML/RTF payloads and used to
//! hand them to the async consumer over an unbounded channel. A copy flood
//! then held every pending snapshot in RAM until persist caught up.
//!
//! Policy (drop-oldest):
//! - At most [`SNAPSHOT_QUEUE_CAPACITY`] events may wait.
//! - Non-oversize queued payloads may hold at most
//!   [`SNAPSHOT_QUEUE_MAX_BYTES`] in total. An accepted oversize capture is
//!   excluded from that sum so a later small event does not evict it, but
//!   follow-ups that themselves fit the budget are still RAM-bounded.
//! - Count overflow evicts FIFO from the front (even an oversize resident).
//!   Byte overflow evicts the oldest non-oversize item.
//! - A single event larger than the byte budget is still accepted after the
//!   queue is emptied: we never refuse the current clipboard, we refuse an
//!   unbounded backlog. A later oversized event replaces that capture.
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
    let item_bytes = item.payload_bytes();
    let mut dropped = 0;
    let mut dropped_bytes = 0;
    let mut evicted = Vec::new();

    if item_bytes > max_bytes {
        // Newest oversized capture wins: refuse an unbounded backlog of
        // large payloads, never the current clipboard.
        while let Some(old) = queue.pop_front() {
            let old_bytes = old.payload_bytes();
            *queued_bytes = queued_bytes.saturating_sub(old_bytes);
            dropped += 1;
            dropped_bytes += old_bytes;
            evicted.push(old);
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
                let Some(old) = queue.pop_front() else {
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
            } else if let Some(index) = queue
                .iter()
                .position(|queued| queued.payload_bytes() <= max_bytes)
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
    }

    impl SizedPayload for FakeSnapshot {
        fn payload_bytes(&self) -> usize {
            self.bytes
        }
    }

    fn fake(id: u32, bytes: usize) -> FakeSnapshot {
        FakeSnapshot { id, bytes }
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
}
