//! Bounded queue with **drop-oldest** overflow semantics.
//!
//! Tokio's `mpsc::channel` blocks the sender when full and `try_send`
//! returns Err — neither matches "drop the oldest entry to make room for
//! the new one", which the spec requires for ingestion buffers during a
//! downstream outage.
//!
//! Simplification: the consumer drains the entire queue at once via
//! [`DropOldest::drain_into`]. Fits both ingestion sinks (vm_writer
//! batches, pg_writer single-shot inserts).

use std::collections::VecDeque;
use std::sync::Mutex;
use tokio::sync::Notify;

/// Bounded queue of owned `T` values with drop-oldest overflow semantics.
pub struct DropOldest<T> {
    cap: usize,
    inner: Mutex<VecDeque<T>>,
    notify: Notify,
}

impl<T> DropOldest<T> {
    /// Construct a new queue with capacity `cap` items. Panics if `cap == 0`.
    pub fn new(cap: usize) -> Self {
        assert!(cap > 0, "queue cap must be > 0");
        Self {
            cap,
            inner: Mutex::new(VecDeque::with_capacity(cap)),
            notify: Notify::new(),
        }
    }

    /// Push an item. Returns `true` if an older item was dropped to make
    /// room.
    pub fn push(&self, item: T) -> bool {
        let mut q = self.inner.lock().unwrap();
        let dropped = if q.len() >= self.cap {
            q.pop_front();
            true
        } else {
            false
        };
        q.push_back(item);
        drop(q);
        self.notify.notify_one();
        dropped
    }

    /// Drain up to `max` items into `out`. Returns the number moved.
    pub fn drain_into(&self, out: &mut Vec<T>, max: usize) -> usize {
        let mut q = self.inner.lock().unwrap();
        let n = q.len().min(max);
        out.extend(q.drain(..n));
        n
    }

    /// Wait until the queue becomes non-empty. Wakes via `notify_one()`.
    pub async fn wait(&self) {
        loop {
            {
                let q = self.inner.lock().unwrap();
                if !q.is_empty() {
                    return;
                }
            }
            self.notify.notified().await;
        }
    }

    /// Current item count (mainly for tests and metrics).
    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().len()
    }

    /// Returns `true` if the queue contains no items.
    pub fn is_empty(&self) -> bool {
        self.inner.lock().unwrap().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn push_under_cap_does_not_drop() {
        let q: DropOldest<u32> = DropOldest::new(3);
        assert!(!q.push(1));
        assert!(!q.push(2));
        assert!(!q.push(3));
        assert_eq!(q.len(), 3);
    }

    #[test]
    fn push_at_cap_drops_oldest() {
        let q: DropOldest<u32> = DropOldest::new(2);
        q.push(1);
        q.push(2);
        assert!(q.push(3));
        let mut out = Vec::new();
        q.drain_into(&mut out, 10);
        assert_eq!(out, vec![2, 3]);
    }

    #[tokio::test]
    async fn wait_returns_after_push() {
        let q: Arc<DropOldest<u32>> = Arc::new(DropOldest::new(4));
        let q2 = q.clone();
        let h = tokio::spawn(async move { q2.wait().await });
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        q.push(7);
        h.await.unwrap();
    }

    #[tokio::test]
    async fn wait_returns_immediately_if_non_empty() {
        let q: Arc<DropOldest<u32>> = Arc::new(DropOldest::new(4));
        q.push(1);
        tokio::time::timeout(std::time::Duration::from_millis(50), q.wait())
            .await
            .expect("wait should not block when non-empty");
    }
}
