#![cfg(feature = "tokio")]

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use eulogy::AsyncDrop;

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
    let guard = Counter { drop_count: count.clone() }.later();

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

    let g1 = Counter { drop_count: count.clone() }.later();
    let g2 = Counter { drop_count: count.clone() }.later();
    let g3 = Counter { drop_count: count.clone() }.later();

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

    let guard = SlowDrop {
        completed: completed.clone(),
        delay: Duration::from_millis(100),
    }
    .later();

    drop(guard);

    // Not done yet — drop is in flight.
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(completed.load(Ordering::SeqCst), 0);

    // Now it should be done.
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(completed.load(Ordering::SeqCst), 1);
}

/// `into_inner` recovers the value without running async_drop.
#[tokio::test]
async fn into_inner_skips_async_drop() {
    let count = Arc::new(AtomicU32::new(0));

    let guard = Counter { drop_count: count.clone() }.later();
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

    struct BlackHoleSpawner;
    impl eulogy::Spawner for BlackHoleSpawner {
        fn spawn<F>(&self, _future: F)
        where
            F: Future<Output = ()> + Send + 'static,
        {
            // Don't actually spawn — simulates task being cancelled.
        }
    }

    let guard = Counter { drop_count: count.clone() }.later_with(&BlackHoleSpawner);
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

    const N: usize = 10;
    let seq = Arc::new(AtomicU32::new(0));
    let mut dropped_positions = Vec::new();
    let mut guards = Vec::new();

    // Build a linear chain of N resources: resource i waits on resource i-1's
    // trigger. Each iteration creates a fresh (wait, trigger) pair and hands
    // the wait forward to the next iteration.
    let mut pending_wait: Option<ordering::DropWait> = None;
    for i in 0..N {
        let dropped_at = Arc::new(AtomicU32::new(0));
        dropped_positions.push(dropped_at.clone());

        let (next_wait, my_trigger) = if i == N - 1 {
            (None, None)
        } else {
            let (w, t) = ordering::setup();
            (Some(w), Some(t))
        };

        let guard = Ordered {
            seq: seq.clone(),
            dropped_at,
            wait: pending_wait.take(),
            _trigger: my_trigger,
        }
        .later();
        guards.push(guard);
        pending_wait = next_wait;
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

/// A parent holds one DropWait; three children each hold a clone of the same
/// DropTrigger. The parent's async_drop must NOT complete until every child
/// has dropped its trigger clone — i.e. all deps have finished.
#[tokio::test]
async fn parent_waits_for_all_trigger_clones() {
    use eulogy::ordering;

    #[derive(Debug)]
    struct Parent {
        wait: Option<ordering::DropWait>,
        cleaned_up: Arc<AtomicU32>,
    }

    impl AsyncDrop for Parent {
        async fn async_drop(self) {
            if let Some(wait) = self.wait {
                wait.wait().await;
            }
            self.cleaned_up.store(1, Ordering::SeqCst);
        }
    }

    #[derive(Debug)]
    struct Child {
        _trigger: ordering::DropTrigger,
        released_at: Arc<AtomicU32>,
        seq: Arc<AtomicU32>,
    }

    impl AsyncDrop for Child {
        async fn async_drop(self) {
            // Simulate some async work before this child is done.
            tokio::time::sleep(Duration::from_millis(20)).await;
            let pos = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
            self.released_at.store(pos, Ordering::SeqCst);
            // _trigger drops here — the parent's wait is only released when
            // every child has hit this point.
        }
    }

    let (wait, trigger) = ordering::setup();
    let seq = Arc::new(AtomicU32::new(0));
    let parent_cleaned = Arc::new(AtomicU32::new(0));

    let parent = Parent {
        wait: Some(wait),
        cleaned_up: parent_cleaned.clone(),
    }
    .later();

    let c1_at = Arc::new(AtomicU32::new(0));
    let c2_at = Arc::new(AtomicU32::new(0));
    let c3_at = Arc::new(AtomicU32::new(0));

    let c1 = Child { _trigger: trigger.clone(), released_at: c1_at.clone(), seq: seq.clone() }.later();
    let c2 = Child { _trigger: trigger.clone(), released_at: c2_at.clone(), seq: seq.clone() }.later();
    let c3 = Child { _trigger: trigger, released_at: c3_at.clone(), seq: seq.clone() }.later();

    // Drop parent first, then children. Parent's async_drop must block on
    // the wait until all three children have completed.
    drop(parent);
    drop(c1);
    drop(c2);

    // Two children released but one still alive → parent must NOT have completed.
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(
        parent_cleaned.load(Ordering::SeqCst),
        0,
        "parent finished before all trigger clones dropped"
    );

    drop(c3);

    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(parent_cleaned.load(Ordering::SeqCst), 1, "parent should have cleaned up");

    // All three children ran; each recorded a position 1..=3.
    assert!(c1_at.load(Ordering::SeqCst) > 0);
    assert!(c2_at.load(Ordering::SeqCst) > 0);
    assert!(c3_at.load(Ordering::SeqCst) > 0);
}
