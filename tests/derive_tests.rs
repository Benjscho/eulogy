use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use eulogy::{later, AsyncDrop};

/// Tracks the order in which fields are dropped via an atomic counter.
#[derive(Debug)]
struct Tracker {
    order: Arc<AtomicU32>,
    dropped_at: Arc<AtomicU32>,
}

impl Tracker {
    fn new(order: Arc<AtomicU32>) -> (Self, Arc<AtomicU32>) {
        let dropped_at = Arc::new(AtomicU32::new(0));
        (Self { order, dropped_at: dropped_at.clone() }, dropped_at)
    }
}

impl AsyncDrop for Tracker {
    async fn async_drop(self) {
        let seq = self.order.fetch_add(1, Ordering::SeqCst) + 1;
        self.dropped_at.store(seq, Ordering::SeqCst);
    }
}

#[derive(Debug, AsyncDrop)]
struct NoDeps {
    #[eulogy]
    a: Tracker,
    #[eulogy]
    b: Tracker,
}

#[derive(Debug, AsyncDrop)]
struct WithOrdering {
    #[eulogy]
    first: Tracker,
    #[eulogy(after = [first])]
    second: Tracker,
    #[eulogy(after = [second])]
    third: Tracker,
}

#[derive(Debug, AsyncDrop)]
struct Diamond {
    #[eulogy]
    a: Tracker,
    #[eulogy]
    b: Tracker,
    #[eulogy(after = [a, b])]
    last: Tracker,
}

#[derive(Debug, AsyncDrop)]
struct SkipsOptOut {
    tracked: Tracker,
    #[eulogy(skip)]
    not_tracked: Tracker,
}

// Generic struct — the derive should synthesize `T: AsyncDrop` automatically.
#[derive(Debug, AsyncDrop)]
struct Generic<T: std::fmt::Debug + Send> {
    #[eulogy]
    inner: T,
}

#[tokio::test]
async fn no_deps_drops_in_declaration_order() {
    let order = Arc::new(AtomicU32::new(0));
    let (a, a_at) = Tracker::new(order.clone());
    let (b, b_at) = Tracker::new(order.clone());

    let guard = later(NoDeps { a, b });
    drop(guard);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    assert_eq!(a_at.load(Ordering::SeqCst), 1);
    assert_eq!(b_at.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn after_enforces_order() {
    let order = Arc::new(AtomicU32::new(0));
    let (first, first_at) = Tracker::new(order.clone());
    let (second, second_at) = Tracker::new(order.clone());
    let (third, third_at) = Tracker::new(order.clone());

    let guard = later(WithOrdering { first, second, third });
    drop(guard);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    assert_eq!(first_at.load(Ordering::SeqCst), 1);
    assert_eq!(second_at.load(Ordering::SeqCst), 2);
    assert_eq!(third_at.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn diamond_deps() {
    let order = Arc::new(AtomicU32::new(0));
    let (a, a_at) = Tracker::new(order.clone());
    let (b, b_at) = Tracker::new(order.clone());
    let (last, last_at) = Tracker::new(order.clone());

    let guard = later(Diamond { a, b, last });
    drop(guard);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // a and b dropped before last
    assert!(a_at.load(Ordering::SeqCst) < last_at.load(Ordering::SeqCst));
    assert!(b_at.load(Ordering::SeqCst) < last_at.load(Ordering::SeqCst));
}

#[tokio::test]
async fn generic_field_gets_async_drop_bound() {
    let order = Arc::new(AtomicU32::new(0));
    let (tracker, dropped_at) = Tracker::new(order.clone());
    let guard = later(Generic { inner: tracker });
    drop(guard);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(dropped_at.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn skip_opts_out_of_async_drop() {
    let order = Arc::new(AtomicU32::new(0));
    let (tracked, tracked_at) = Tracker::new(order.clone());
    let (not_tracked, not_tracked_at) = Tracker::new(order.clone());

    let guard = later(SkipsOptOut {
        tracked,
        not_tracked,
    });
    drop(guard);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    assert_eq!(tracked_at.load(Ordering::SeqCst), 1);
    assert_eq!(not_tracked_at.load(Ordering::SeqCst), 0, "#[eulogy(skip)] field should not have async_drop called");
    assert_eq!(order.load(Ordering::SeqCst), 1); // only one async_drop called
    assert_eq!(order.load(Ordering::SeqCst), 1); // only one async_drop called
}
