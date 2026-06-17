//! Caching for the expensive whole-chain aggregates.
//!
//! `/v2/stats` counts every swap, deposit and withdrawal since genesis. The query set is
//! identical for every caller and the answer can only change when a block lands, so serving it
//! fresh to each request is pure waste — upstream caches it for the same reason.
//!
//! Invalidation is by committed height rather than by a clock: an entry computed at height N is
//! exactly right until N+1 arrives, so there is no staleness window to pick and nothing to tune.
//! On a chain with five-second blocks a time-based TTL would be strictly worse — either stale or
//! pointless.

use std::sync::Arc;

use tokio::sync::Mutex;

/// A value cached against the block height it was computed at.
pub struct HeightCache<T> {
    inner: Arc<Mutex<Option<Entry<T>>>>,
}

struct Entry<T> {
    height: i64,
    value: Arc<T>,
}

impl<T> Clone for HeightCache<T> {
    fn clone(&self) -> Self {
        HeightCache {
            inner: self.inner.clone(),
        }
    }
}

impl<T> Default for HeightCache<T> {
    fn default() -> Self {
        HeightCache::new()
    }
}

impl<T> HeightCache<T> {
    pub fn new() -> HeightCache<T> {
        HeightCache {
            inner: Arc::new(Mutex::new(None)),
        }
    }

    /// Return the cached value for `height`, or compute and store it.
    ///
    /// The lock is held across the computation on purpose. Several requests arriving together
    /// for a cold cache would otherwise all run the same expensive scan; this way the first runs
    /// it and the rest wait for its result. Requests for a *stale* height do the same, which is
    /// the behaviour you want when a new block lands under load.
    pub async fn get_or_compute<F, Fut, E>(&self, height: i64, compute: F) -> Result<Arc<T>, E>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T, E>>,
    {
        let mut guard = self.inner.lock().await;

        if let Some(entry) = guard.as_ref() {
            if entry.height == height {
                return Ok(entry.value.clone());
            }
        }

        let value = Arc::new(compute().await?);
        *guard = Some(Entry {
            height,
            value: value.clone(),
        });
        Ok(value)
    }

    /// The height currently cached, if any. For tests and diagnostics.
    pub async fn cached_height(&self) -> Option<i64> {
        self.inner.lock().await.as_ref().map(|e| e.height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn the_first_call_computes() {
        let cache: HeightCache<i32> = HeightCache::new();
        let v = cache
            .get_or_compute(1, || async { Ok::<_, ()>(42) })
            .await
            .unwrap();
        assert_eq!(*v, 42);
        assert_eq!(cache.cached_height().await, Some(1));
    }

    #[tokio::test]
    async fn the_same_height_is_served_from_cache() {
        let cache: HeightCache<i32> = HeightCache::new();
        let calls = AtomicUsize::new(0);

        for _ in 0..5 {
            let v = cache
                .get_or_compute(7, || async {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, ()>(1)
                })
                .await
                .unwrap();
            assert_eq!(*v, 1);
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1, "should have computed once");
    }

    #[tokio::test]
    async fn a_new_height_recomputes() {
        let cache: HeightCache<i64> = HeightCache::new();

        let a = cache
            .get_or_compute(1, || async { Ok::<_, ()>(100) })
            .await
            .unwrap();
        let b = cache
            .get_or_compute(2, || async { Ok::<_, ()>(200) })
            .await
            .unwrap();

        assert_eq!(*a, 100);
        assert_eq!(*b, 200);
        assert_eq!(cache.cached_height().await, Some(2));
    }

    #[tokio::test]
    async fn a_failed_computation_is_not_cached() {
        // Otherwise one transient database error would be served for a whole block.
        let cache: HeightCache<i32> = HeightCache::new();

        let err = cache
            .get_or_compute(1, || async { Err::<i32, &str>("boom") })
            .await;
        assert!(err.is_err());
        assert_eq!(cache.cached_height().await, None);

        let ok = cache
            .get_or_compute(1, || async { Ok::<_, &str>(5) })
            .await
            .unwrap();
        assert_eq!(*ok, 5);
    }

    #[tokio::test]
    async fn concurrent_callers_share_one_computation() {
        let cache: HeightCache<i32> = HeightCache::new();
        let calls = Arc::new(AtomicUsize::new(0));

        let tasks: Vec<_> = (0..8)
            .map(|_| {
                let cache = cache.clone();
                let calls = calls.clone();
                tokio::spawn(async move {
                    cache
                        .get_or_compute(3, || async move {
                            calls.fetch_add(1, Ordering::SeqCst);
                            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                            Ok::<_, ()>(9)
                        })
                        .await
                        .unwrap()
                })
            })
            .collect();

        for t in tasks {
            assert_eq!(*t.await.unwrap(), 9);
        }
        // The point of holding the lock across the computation: a thundering herd on a cold
        // cache does one scan, not eight.
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
