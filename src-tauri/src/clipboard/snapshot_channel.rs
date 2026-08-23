//! Async transport for the SBS-1032 bounded snapshot queue.
//!
//! The drop-oldest policy lives in [`super::snapshot_queue`] so it can be
//! proven with `rustc --test` on Linux. This wrapper is the production
//! sender/receiver that replaced `tokio::sync::mpsc::unbounded_channel`.

use super::snapshot_queue::{
    dequeue, enqueue, EnqueueOutcome, SizedPayload, SNAPSHOT_QUEUE_CAPACITY,
    SNAPSHOT_QUEUE_MAX_BYTES,
};
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::Notify;

struct Inner<T> {
    items: VecDeque<T>,
    bytes: usize,
    receiver_gone: bool,
    senders_gone: bool,
}

impl<T: SizedPayload> Inner<T> {
    // Methods take `&mut self` so we do not split-borrow a MutexGuard
    // (parking_lot's DerefMut is not disjoint-field aware).
    fn push(&mut self, item: T) -> EnqueueOutcome<T> {
        enqueue(
            &mut self.items,
            &mut self.bytes,
            item,
            SNAPSHOT_QUEUE_CAPACITY,
            SNAPSHOT_QUEUE_MAX_BYTES,
        )
    }

    fn pop(&mut self) -> Option<T> {
        dequeue(&mut self.items, &mut self.bytes)
    }
}

struct Shared<T> {
    inner: Mutex<Inner<T>>,
    notify: Notify,
}

pub(super) struct Sender<T> {
    shared: Arc<Shared<T>>,
}

pub(super) struct Receiver<T> {
    shared: Arc<Shared<T>>,
}

pub(super) fn channel<T: SizedPayload>() -> (Sender<T>, Receiver<T>) {
    let shared = Arc::new(Shared {
        inner: Mutex::new(Inner {
            items: VecDeque::new(),
            bytes: 0,
            receiver_gone: false,
            senders_gone: false,
        }),
        notify: Notify::new(),
    });
    (
        Sender {
            shared: Arc::clone(&shared),
        },
        Receiver { shared },
    )
}

impl<T: SizedPayload> Sender<T> {
    /// Queue an item and handle evictions before the receiver is woken.
    ///
    /// The ordering matters for clipboard ignore state: waking first lets the
    /// consumer process a later same-hash item before an evicted echo releases
    /// its ignore generation (SBS-1039).
    pub(super) fn send(
        &self,
        item: T,
        on_evicted: impl FnOnce(&[T]),
    ) -> Result<EnqueueOutcome<T>, ()> {
        let mut inner = self.shared.inner.lock();
        if inner.receiver_gone {
            return Err(());
        }
        let outcome = inner.push(item);
        if let EnqueueOutcome::QueuedAfterDrop { evicted, .. } = &outcome {
            on_evicted(evicted);
        }
        drop(inner);
        self.shared.notify.notify_one();
        Ok(outcome)
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        self.shared.inner.lock().senders_gone = true;
        self.shared.notify.notify_one();
    }
}

impl<T: SizedPayload> Receiver<T> {
    pub(super) async fn recv(&self) -> Option<T> {
        loop {
            {
                let mut inner = self.shared.inner.lock();
                if let Some(item) = inner.pop() {
                    return Some(item);
                }
                if inner.senders_gone {
                    return None;
                }
            }
            self.shared.notify.notified().await;
        }
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        self.shared.inner.lock().receiver_gone = true;
    }
}

#[cfg(test)]
mod tests {
    use super::super::snapshot_queue::{EnqueueOutcome, SizedPayload, SNAPSHOT_QUEUE_CAPACITY};
    use super::channel;

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

    #[test]
    fn send_fails_after_the_receiver_is_dropped() {
        let (tx, rx) = channel::<FakeSnapshot>();
        drop(rx);
        assert_eq!(tx.send(fake(1, 1), |_| {}), Err(()));
    }

    #[tokio::test]
    async fn recv_delivers_enqueued_items_in_order() {
        let (tx, rx) = channel();
        assert_eq!(tx.send(fake(1, 4), |_| {}), Ok(EnqueueOutcome::Queued));
        assert_eq!(tx.send(fake(2, 4), |_| {}), Ok(EnqueueOutcome::Queued));
        assert_eq!(rx.recv().await, Some(fake(1, 4)));
        assert_eq!(rx.recv().await, Some(fake(2, 4)));
    }

    #[tokio::test]
    async fn recv_ends_when_the_sender_is_dropped() {
        let (tx, rx) = channel::<FakeSnapshot>();
        drop(tx);
        assert_eq!(rx.recv().await, None);
    }

    #[tokio::test]
    async fn the_channel_keeps_the_newest_items_after_a_count_flood() {
        let (tx, rx) = channel();
        let flood = SNAPSHOT_QUEUE_CAPACITY + 5;
        for id in 0..flood {
            tx.send(fake(id as u32, 1), |_| {})
                .expect("receiver is still alive");
        }
        let first = rx.recv().await.expect("the newest window must remain");
        assert_eq!(first.id, 5);
        drop(tx);
        let mut last = first;
        while let Some(item) = rx.recv().await {
            last = item;
        }
        assert_eq!(last.id, flood as u32 - 1);
    }

    #[test]
    fn send_returns_evicted_items_on_count_flood() {
        let (tx, _rx) = channel();
        for id in 0..SNAPSHOT_QUEUE_CAPACITY {
            assert_eq!(
                tx.send(fake(id as u32, 1), |_| {}),
                Ok(EnqueueOutcome::Queued),
                "the first {SNAPSHOT_QUEUE_CAPACITY} items must fit"
            );
        }
        let mut handled = Vec::new();
        match tx.send(fake(SNAPSHOT_QUEUE_CAPACITY as u32, 1), |evicted| {
            handled.extend(evicted.iter().map(|item| item.id));
        }) {
            Ok(EnqueueOutcome::QueuedAfterDrop {
                evicted, dropped, ..
            }) => {
                assert_eq!(dropped, 1);
                assert_eq!(
                    evicted[0].id, 0,
                    "fail-without-fix: send discarded the evicted item (SBS-1039)"
                );
            }
            other => panic!("expected eviction of the oldest item, got {other:?}"),
        }
        assert_eq!(handled, vec![0]);
    }
}
