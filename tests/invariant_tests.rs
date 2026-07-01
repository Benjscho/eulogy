#![cfg(feature = "tokio")]

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use eulogy::{later, AsyncDrop};

// -- Helpers --

#[derive(Debug)]
struct Counter {
    drop_count: Arc<AtomicU32>,
}

impl AsyncDrop for Counter {
    async fn async_drop(self) {
        self.drop_count.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Debug)]
struct SlowDrop {
    completed: Arc<AtomicU32>,
    delay: Duration,
}

impl AsyncDrop for SlowDrop {
    async fn async_drop(self) {
        tokio::time::sleep(self.delay).await;
        self.completed.fetch_add(1, Ordering::SeqCst);
    }
}

// -- Tests --

/// Drop runs exactly once, even when the guard is moved between scopes.
#[tokio::test]
async fn drop_runs_exactly_once() {
    let count = Arc::new(AtomicU32::new(0));
    let guard = later(Counter { drop_count: count.clone() });

    // Move the guard into a new scope.
    let moved = guard;
    drop(moved);

    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

/// Multiple guards each drop exactly once.
#[tokio::test]
async fn multiple_guards_each_drop_once() {
    let count = Arc::new(AtomicU32::new(0));

    let g1 = later(Counter { drop_count: count.clone() });
    let g2 = later(Counter { drop_count: count.clone() });
    let g3 = later(Counter { drop_count: count.clone() });

    drop(g1);
    drop(g2);
    drop(g3);

    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(count.load(Ordering::SeqCst), 3);
}

/// Async drop runs to completion (not just started).
#[tokio::test]
async fn drop_completes_before_task_exits() {
    let completed = Arc::new(AtomicU32::new(0));

    let guard = later(SlowDrop {
        completed: completed.clone(),
        delay: Duration::from_millis(100),
    });

    drop(guard);

    // Not done yet — drop is in flight.
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(completed.load(Ordering::SeqCst), 0);

    // Now it should be done.
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(completed.load(Ordering::SeqCst), 1);
}

#[test]
fn chain_endpoint_shape() {
    use eulogy::ordering;

    let empty = ordering::chain(0);
    assert!(empty.is_empty());

    let single = ordering::chain(1);
    assert_eq!(single.len(), 1);
    assert!(single[0].wait.is_none());
    assert!(single[0].trigger.is_none());

    let pair = ordering::chain(2);
    assert_eq!(pair.len(), 2);
    assert!(pair[0].wait.is_none());
    assert!(pair[0].trigger.is_some());
    assert!(pair[1].wait.is_some());
    assert!(pair[1].trigger.is_none());

    let triple = ordering::chain(3);
    // Middle link has both.
    assert!(triple[1].wait.is_some());
    assert!(triple[1].trigger.is_some());
}

/// `into_inner` recovers the value without running async_drop.
#[tokio::test]
async fn into_inner_skips_async_drop() {
    let count = Arc::new(AtomicU32::new(0));

    let guard = later(Counter { drop_count: count.clone() });
    let recovered = guard.into_inner();

    // Give the spawned task a chance to notice the sender was dropped.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(count.load(Ordering::SeqCst), 0, "async_drop must not run");

    // We still hold `recovered` — sync drop when it goes out of scope. No leak.
    drop(recovered);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(count.load(Ordering::SeqCst), 0, "sync drop doesn't call async_drop either");
}

/// If the receiver task is gone (simulating runtime shutdown), Drop doesn't panic.
#[tokio::test]
async fn cancellation_does_not_panic() {
    let count = Arc::new(AtomicU32::new(0));

    // Create a guard with a custom spawner that drops the task immediately.
    use std::future::Future;
    use std::pin::Pin;

    struct BlackHoleSpawner;
    impl eulogy::Spawner for BlackHoleSpawner {
        fn spawn(&self, _future: Pin<Box<dyn Future<Output = ()> + Send>>) {
            // Don't actually spawn — simulates task being cancelled.
        }
    }

    let guard = eulogy::later_with(Counter { drop_count: count.clone() }, &BlackHoleSpawner);
    drop(guard); // Should not panic.

    tokio::time::sleep(Duration::from_millis(10)).await;
    // async_drop was never called because the task never ran.
    assert_eq!(count.load(Ordering::SeqCst), 0);
}

/// Ordering is respected under contention: many resources with deps dropped at once.
#[tokio::test]
async fn ordering_under_contention() {
    use eulogy::ordering;

    #[derive(Debug)]
    struct Ordered {
        seq: Arc<AtomicU32>,
        dropped_at: Arc<AtomicU32>,
        wait: Option<ordering::DropWait>,
        _trigger: Option<ordering::DropTrigger>,
    }

    impl AsyncDrop for Ordered {
        async fn async_drop(self) {
            if let Some(wait) = self.wait {
                wait.wait().await;
            }
            let pos = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
            self.dropped_at.store(pos, Ordering::SeqCst);
        }
    }

    let seq = Arc::new(AtomicU32::new(0));
    let mut dropped_positions = Vec::new();
    let mut guards = Vec::new();

    // ordering::chain sugars the boilerplate: each Link i has the wait for
    // link i-1's trigger and the trigger that releases link i+1's wait.
    let links = ordering::chain(10);
    for link in links {
        let dropped_at = Arc::new(AtomicU32::new(0));
        dropped_positions.push(dropped_at.clone());

        let guard = later(Ordered {
            seq: seq.clone(),
            dropped_at,
            wait: link.wait,
            _trigger: link.trigger,
        });

        guards.push(guard);
    }

    // Drop all at once (reverse order to stress ordering).
    for guard in guards.into_iter().rev() {
        drop(guard);
    }

    tokio::time::sleep(Duration::from_millis(300)).await;

    // Verify: each resource dropped after its predecessor.
    for i in 1..10 {
        let prev = dropped_positions[i - 1].load(Ordering::SeqCst);
        let curr = dropped_positions[i].load(Ordering::SeqCst);
        assert!(
            prev < curr,
            "resource {} (pos {}) should drop before resource {} (pos {})",
            i - 1, prev, i, curr
        );
    }
}
