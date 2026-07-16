#![cfg(all(feature = "tokio", feature = "derive"))]

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use eulogy::AsyncDrop;

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

// Two slow drops with no ordering; used to prove Vec<T> drops concurrently.
#[derive(Debug)]
struct SlowTracker {
    delay: std::time::Duration,
    done: Arc<AtomicU32>,
}

impl AsyncDrop for SlowTracker {
    async fn async_drop(self) {
        tokio::time::sleep(self.delay).await;
        self.done.fetch_add(1, Ordering::SeqCst);
    }
}

// Ergonomics regression test: none of these fields are `#[eulogy(skip)]`,
// which would have failed to compile before std_impls existed.
#[derive(Debug, AsyncDrop)]
struct WithStdFields {
    #[eulogy]
    name: String,
    #[eulogy]
    path: std::path::PathBuf,
    #[eulogy]
    count: u32,
}

#[derive(Debug, AsyncDrop)]
struct WithOption {
    #[eulogy]
    maybe: Option<Tracker>,
}

#[derive(Debug, AsyncDrop)]
struct WithBox {
    #[eulogy]
    boxed: Box<Tracker>,
}

#[derive(Debug, AsyncDrop)]
struct WithVec {
    #[eulogy]
    many: Vec<SlowTracker>,
}

#[derive(Debug, AsyncDrop)]
struct WithTuple {
    #[eulogy]
    pair: (Tracker, Tracker),
}

#[tokio::test]
async fn std_fields_compile_and_drop_without_skip() {
    let guard = WithStdFields {
        name: "resource".to_string(),
        path: std::path::PathBuf::from("/tmp/resource"),
        count: 42,
    }
    .later();
    drop(guard);
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
}

#[tokio::test]
async fn option_some_drops_inner() {
    let order = Arc::new(AtomicU32::new(0));
    let (tracker, dropped_at) = Tracker::new(order.clone());

    let guard = WithOption { maybe: Some(tracker) }.later();
    drop(guard);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    assert_eq!(dropped_at.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn option_none_is_noop() {
    let guard = WithOption { maybe: None }.later();
    drop(guard);
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
}

#[tokio::test]
async fn box_unboxes_and_drops() {
    let order = Arc::new(AtomicU32::new(0));
    let (tracker, dropped_at) = Tracker::new(order.clone());

    let guard = WithBox { boxed: Box::new(tracker) }.later();
    drop(guard);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    assert_eq!(dropped_at.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn vec_drops_elements_concurrently() {
    let done = Arc::new(AtomicU32::new(0));
    let delay = std::time::Duration::from_millis(100);
    let many = vec![
        SlowTracker { delay, done: done.clone() },
        SlowTracker { delay, done: done.clone() },
        SlowTracker { delay, done: done.clone() },
    ];

    let guard = WithVec { many }.later();

    let start = std::time::Instant::now();
    drop(guard);

    while done.load(Ordering::SeqCst) < 3 && start.elapsed() < std::time::Duration::from_millis(500) {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let elapsed = start.elapsed();

    assert_eq!(done.load(Ordering::SeqCst), 3);
    assert!(
        elapsed < std::time::Duration::from_millis(250),
        "expected concurrent drop (< ~1x delay), got {:?}",
        elapsed
    );
}

#[tokio::test]
async fn tuple_field_drops_both_elements() {
    let order = Arc::new(AtomicU32::new(0));
    let (a, a_at) = Tracker::new(order.clone());
    let (b, b_at) = Tracker::new(order.clone());

    let guard = WithTuple { pair: (a, b) }.later();
    drop(guard);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    assert!(a_at.load(Ordering::SeqCst) > 0);
    assert!(b_at.load(Ordering::SeqCst) > 0);
    assert_eq!(order.load(Ordering::SeqCst), 2);
}
