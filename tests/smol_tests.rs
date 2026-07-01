//! Mirrors the core invariants under the `smol` runtime feature.
//! Run with: cargo test --no-default-features --features smol --features derive

#![cfg(all(feature = "smol", not(feature = "tokio")))]

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use eulogy::{later, AsyncDrop};

#[derive(Debug)]
struct Counter {
    count: Arc<AtomicU32>,
}

impl AsyncDrop for Counter {
    async fn async_drop(self) {
        self.count.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Debug)]
struct SlowDrop {
    completed: Arc<AtomicU32>,
    delay: Duration,
}

impl AsyncDrop for SlowDrop {
    async fn async_drop(self) {
        smol::Timer::after(self.delay).await;
        self.completed.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn drop_runs_once_smol() {
    smol::block_on(async {
        let count = Arc::new(AtomicU32::new(0));
        let guard = later(Counter { count: count.clone() });
        drop(guard);
        smol::Timer::after(Duration::from_millis(50)).await;
        assert_eq!(count.load(Ordering::SeqCst), 1);
    });
}

#[test]
fn multiple_guards_smol() {
    smol::block_on(async {
        let count = Arc::new(AtomicU32::new(0));
        let g1 = later(Counter { count: count.clone() });
        let g2 = later(Counter { count: count.clone() });
        let g3 = later(Counter { count: count.clone() });
        drop(g1);
        drop(g2);
        drop(g3);
        smol::Timer::after(Duration::from_millis(50)).await;
        assert_eq!(count.load(Ordering::SeqCst), 3);
    });
}

#[test]
fn slow_drop_completes_smol() {
    smol::block_on(async {
        let completed = Arc::new(AtomicU32::new(0));
        let guard = later(SlowDrop {
            completed: completed.clone(),
            delay: Duration::from_millis(50),
        });
        drop(guard);
        smol::Timer::after(Duration::from_millis(150)).await;
        assert_eq!(completed.load(Ordering::SeqCst), 1);
    });
}

#[test]
fn into_inner_smol() {
    smol::block_on(async {
        let count = Arc::new(AtomicU32::new(0));
        let guard = later(Counter { count: count.clone() });
        let _recovered = guard.into_inner();
        smol::Timer::after(Duration::from_millis(50)).await;
        assert_eq!(count.load(Ordering::SeqCst), 0);
    });
}

// Derive under smol.
#[cfg(feature = "derive")]
mod derive_smol {
    use super::*;
    use eulogy::AsyncDrop;

    #[derive(Debug, AsyncDrop)]
    struct Parent {
        first: Counter,
        #[eulogy(after = [first])]
        second: Counter,
    }

    #[test]
    fn derive_ordering_smol() {
        smol::block_on(async {
            let count = Arc::new(AtomicU32::new(0));
            let guard = later(Parent {
                first: Counter { count: count.clone() },
                second: Counter { count: count.clone() },
            });
            drop(guard);
            smol::Timer::after(Duration::from_millis(50)).await;
            assert_eq!(count.load(Ordering::SeqCst), 2);
        });
    }
}
