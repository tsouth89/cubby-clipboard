//! Bounded in-process snapshot queue (SBS-1032).
//!
//! The native listener materializes full PNG/HTML/RTF payloads and used to
//! hand them to the async consumer over an unbounded channel. A copy flood
//! then held every pending snapshot in RAM until persist caught up.
//!
//! Policy (drop-oldest):
//! - At most [`SNAPSHOT_QUEUE_CAPACITY`] events may wait.
//! - Those events may hold at most [`SNAPSHOT_QUEUE_MAX_BYTES`] of payload.
//! - A new event that would exceed either bound evicts the oldest pending
//!   event(s) until it fits. The newest copy is preferred because the
//!   clipboard has already moved on.
//! - A single event larger than the byte budget is still accepted after the
//!   queue is emptied: we never refuse the current clipboard, we refuse an
//!   unbounded backlog.
//! - A 100-copy text burst (the reliability contract) stays under both
//!   budgets, so capture order is preserved under normal load.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EnqueueOutcome {
    Queued,
    QueuedAfterDrop {
        dropped: usize,
        dropped_bytes: usize,
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
) -> EnqueueOutcome {
    debug_assert!(
        capacity >= 1,
        "SBS-1032: the queue must be able to hold the current capture"
    );
    let item_bytes = item.payload_bytes();
    let mut dropped = 0;
    let mut dropped_bytes = 0;

    loop {
        let count_ok = queue.len() < capacity;
        // An empty queue accepts even an oversized item so one screenshot
        // cannot be refused. A non-empty queue evicts until the new item
        // fits the byte budget.
        let bytes_ok = queue.is_empty() || queued_bytes.saturating_add(item_bytes) <= max_bytes;
        if count_ok && bytes_ok {
            break;
        }
        let Some(old) = queue.pop_front() else {
            break;
        };
        let old_bytes = old.payload_bytes();
        *queued_bytes = queued_bytes.saturating_sub(old_bytes);
        dropped += 1;
        dropped_bytes += old_bytes;
    }

    *queued_bytes = queued_bytes.saturating_add(item_bytes);
    queue.push_back(item);

    if dropped == 0 {
        EnqueueOutcome::Queued
    } else {
        EnqueueOutcome::QueuedAfterDrop {
            dropped,
            dropped_bytes,
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
                dropped_bytes: 8
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
                dropped_bytes: 20
            }
        );
        assert_eq!(ids(&queue), vec![3]);
        assert_eq!(bytes, 50);
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

    #[test]
    fn the_shipped_capacity_covers_the_100_copy_reliability_burst() {
        assert!(
            SNAPSHOT_QUEUE_CAPACITY >= 100,
            "the reliability contract captures a 100-copy burst; the queue must hold it"
        );
    }
}
