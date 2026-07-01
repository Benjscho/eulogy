//! # Eulogy
//!
//! Give your data a good send off.
//!
//! Userspace async drop for Rust. Provides an [`AsyncDrop`] trait and a
//! [`DropLater`] guard that runs async cleanup when a value is dropped.
//!
//! ## Usage
//!
//! Enable exactly one runtime feature in your final binary:
//!
//! ```toml
//! eulogy = { version = "0.1", features = ["tokio"] }
//! ```
//!
//! Libraries should depend on `eulogy` with no features — the binary picks
//! the runtime:
//!
//! ```toml
//! eulogy = "0.1"
//! ```
//!
//! The API is the same regardless of runtime:
//!
//! ```ignore
//! let guard = eulogy::later(my_value);
//! ```
//!
//! ## Ordering
//!
//! The [`ordering`] module provides primitives to enforce drop order between
//! related resources (e.g. a parent directory must outlive its children).

use std::future::Future;
use std::pin::Pin;
use std::ops;

#[cfg(all(feature = "tokio", feature = "smol"))]
compile_error!(
    "eulogy: enable only one of `tokio` or `smol` — both runtimes active in the \
     same binary means later() silently picks tokio and any smol-context drop \
     panics. If a transitive dep is enabling the other feature, disable its \
     default features."
);

/// Re-export the derive macro when the `derive` feature is enabled.
#[cfg(feature = "derive")]
pub use eulogy_derive::AsyncDrop;

/// A type that requires async cleanup.
///
/// # Requirements
///
/// The trait requires `Send`; [`later`]/[`later_with`] additionally require
/// `'static`. Both are needed to move the value into a spawned task on the
/// runtime executor. This means:
///
/// - `!Send` types like `Rc<T>`, non-Send I/O handles, or values holding
///   raw pointers can't use [`later`]. There is currently no `LocalAsyncDrop`
///   variant — single-threaded / `LocalSet`-only cleanup is not supported.
/// - Non-`'static` values (anything borrowing) can't be wrapped in a guard.
///   Own the data, or `Arc` it before wrapping.
///
/// If either constraint is a blocker for you, implement [`Spawner`] yourself
/// and reach for [`later_with`] — the `AsyncDrop` trait itself only needs
/// `Send` today, but that may loosen further.
pub trait AsyncDrop: Send {
    /// Perform async cleanup, consuming the value.
    fn async_drop(self) -> impl Future<Output = ()> + Send;
}

/// Spawns a future onto an async runtime.
///
/// You can implement this to use a custom runtime with [`later_with`].
pub trait Spawner {
    fn spawn(&self, future: Pin<Box<dyn Future<Output = ()> + Send>>);
}

/// A guard that runs [`AsyncDrop`] on the contained value when dropped.
///
/// Access the inner value via `Deref`/`DerefMut`.
///
/// # Ordering between sibling guards
///
/// Dropping guards in source order does **not** guarantee their `async_drop`
/// futures execute in that order:
///
/// ```ignore
/// let a = later(A { .. });
/// let b = later(B { .. });
/// drop(a);   // enqueues A for cleanup
/// drop(b);   // enqueues B for cleanup
/// // A's async_drop and B's async_drop may run in either order or interleaved
/// ```
///
/// Each guard is cleaned up by its own spawned task; the runtime is free to
/// schedule them however it likes. For a struct with fields that must be
/// dropped in a specific order, use `#[derive(AsyncDrop)]` with
/// `#[eulogy(after = [...])]`. For sibling guards, use [`ordering::setup`].
#[must_use = "dropping the guard is what triggers async_drop — keep it alive until you want cleanup"]
pub struct DropLater<T: AsyncDrop + 'static> {
    value: Option<T>,
    dropper: Option<async_channel::Sender<T>>,
}

impl<T: AsyncDrop + 'static> DropLater<T> {
    fn new(value: T, dropper: async_channel::Sender<T>) -> Self {
        Self {
            value: Some(value),
            dropper: Some(dropper),
        }
    }

    /// Recover the inner value without running `async_drop`.
    ///
    /// The spawned drop task learns via a channel close that the value is
    /// gone and exits cleanly — nothing leaks. Use this when you need to
    /// hand ownership of `T` elsewhere and take responsibility for cleanup
    /// yourself.
    pub fn into_inner(mut self) -> T {
        let value = self.value.take().expect("value is present until drop");
        // Drop the sender explicitly so the spawned task's `rx.recv().await`
        // resolves to Err and the task exits without touching `T`.
        drop(self.dropper.take());
        // Skip our Drop impl — its work is already done above.
        std::mem::forget(self);
        value
    }
}

impl<T: AsyncDrop + 'static> Drop for DropLater<T> {
    fn drop(&mut self) {
        let value = self.value.take().expect("drop runs once");
        let sender = self.dropper.take().expect("drop runs once");
        if sender.try_send(value).is_err() {
            tracing::trace!("leaking resource (drop task canceled)");
        }
    }
}

impl<T: AsyncDrop + 'static> ops::Deref for DropLater<T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        self.value.as_ref().unwrap()
    }
}

impl<T: AsyncDrop + 'static> ops::DerefMut for DropLater<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.value.as_mut().unwrap()
    }
}

impl<T: AsyncDrop + std::fmt::Debug + 'static> std::fmt::Debug for DropLater<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DropLater")
            .field("value", &self.value)
            .finish()
    }
}

/// Wrap a value so its [`AsyncDrop`] runs when the guard is dropped.
///
/// Uses the runtime enabled via feature flag. Requires either the `tokio` or
/// `smol` feature; without one of these, `later` is not defined and calls
/// will fail to compile (use [`later_with`] for a custom [`Spawner`]).
/// Libraries call this without caring which runtime the binary chose.
///
/// # Panics
///
/// Must be called from within a runtime context. With the `tokio` feature,
/// calling this outside a `#[tokio::main]` / `#[tokio::test]` / `Runtime::block_on`
/// scope panics with "there is no reactor running" — the same rule as
/// `tokio::spawn`. Tests using `#[test]` instead of `#[tokio::test]` are the
/// most common offender. Same caveat for `smol::spawn` under the `smol` feature.
#[cfg(feature = "tokio")]
pub fn later<T: AsyncDrop + 'static>(value: T) -> DropLater<T> {
    later_with(value, &TokioSpawner)
}

/// Wrap a value so its [`AsyncDrop`] runs when the guard is dropped.
///
/// See the [`tokio`-flavored variant](later) for details. Defined when the
/// `smol` feature is enabled and `tokio` is not.
///
/// # Panics
///
/// Must be called from within a `smol::block_on` (or `smol::Executor`) scope.
#[cfg(all(feature = "smol", not(feature = "tokio")))]
pub fn later<T: AsyncDrop + 'static>(value: T) -> DropLater<T> {
    later_with(value, &SmolSpawner)
}

/// Wrap a value so its [`AsyncDrop`] runs when the guard is dropped,
/// using a custom [`Spawner`].
pub fn later_with<T: AsyncDrop + 'static>(value: T, spawner: &impl Spawner) -> DropLater<T> {
    let (tx, rx) = async_channel::bounded(1);
    let guard = DropLater::new(value, tx);
    spawner.spawn(Box::pin(async move {
        if let Ok(value) = rx.recv().await {
            value.async_drop().await;
        }
    }));
    guard
}

/// Runtime-agnostic helpers used by generated derive code. Not part of the
/// stable API surface — users should not depend on these directly.
#[doc(hidden)]
pub mod __private {
    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    /// Await a fixed set of futures concurrently. Returns when every future
    /// has resolved. Used by the derive to drop independent fields in parallel.
    pub async fn join_all<F>(futs: Vec<F>)
    where
        F: Future<Output = ()>,
    {
        struct JoinAll<F: Future<Output = ()>> {
            futs: Vec<Option<Pin<Box<F>>>>,
        }

        impl<F: Future<Output = ()>> Future for JoinAll<F> {
            type Output = ();
            fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
                let mut all_done = true;
                for slot in self.futs.iter_mut() {
                    if let Some(fut) = slot.as_mut() {
                        match fut.as_mut().poll(cx) {
                            Poll::Ready(()) => *slot = None,
                            Poll::Pending => all_done = false,
                        }
                    }
                }
                if all_done { Poll::Ready(()) } else { Poll::Pending }
            }
        }

        JoinAll {
            futs: futs.into_iter().map(|f| Some(Box::pin(f))).collect(),
        }
        .await;
    }
}

// --- Runtime spawners ---

#[cfg(feature = "tokio")]
#[derive(Debug, Clone, Copy)]
struct TokioSpawner;

#[cfg(feature = "tokio")]
impl Spawner for TokioSpawner {
    fn spawn(&self, future: Pin<Box<dyn Future<Output = ()> + Send>>) {
        tokio::spawn(future);
    }
}

#[cfg(feature = "smol")]
#[derive(Debug, Clone, Copy)]
struct SmolSpawner;

#[cfg(feature = "smol")]
impl Spawner for SmolSpawner {
    fn spawn(&self, future: Pin<Box<dyn Future<Output = ()> + Send>>) {
        smol::spawn(future).detach();
    }
}

/// Drop ordering primitives.
///
/// Use [`setup`] to create a `(DropWait, DropTrigger)` pair. A resource that
/// must wait for dependents to drop first holds a `DropWait` and calls
/// `wait().await` in its `async_drop`. Dependents hold a `DropTrigger` (or
/// clone of one); when all triggers are dropped the wait completes.
pub mod ordering {
    /// Create a linked `(DropWait, DropTrigger)` pair.
    pub fn setup() -> (DropWait, DropTrigger) {
        let (tx, rx) = async_channel::bounded(1);
        (DropWait(rx), DropTrigger(tx))
    }

    /// Awaits until all associated [`DropTrigger`]s have been dropped.
    #[derive(Debug)]
    pub struct DropWait(async_channel::Receiver<()>);

    impl DropWait {
        /// Block until all associated triggers are dropped.
        pub async fn wait(self) {
            let _ = self.0.recv().await;
        }
    }

    /// Hold this in a dependent resource. When all clones are dropped, the
    /// associated [`DropWait`] completes.
    #[derive(Debug, Clone)]
    pub struct DropTrigger(#[allow(dead_code)] async_channel::Sender<()>);

    /// One position in a [`chain`]. Each resource in the chain holds a
    /// `Link` and passes its `wait`/`trigger` into whatever fields need them.
    #[derive(Debug)]
    pub struct Link {
        /// Some for every link except the first — the resource waits on this
        /// before performing its own cleanup.
        pub wait: Option<DropWait>,
        /// Some for every link except the last — dropping the resource
        /// releases the next link's wait.
        pub trigger: Option<DropTrigger>,
    }

    /// Build a chain of `n` links, each waiting on the previous.
    ///
    /// Link 0 has `wait: None`; link `n-1` has `trigger: None`. Every
    /// middle link has both. Wire each link into a resource so that
    /// resource `i` finishes cleanup before resource `i+1` starts.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let mut chain = ordering::chain(3).into_iter();
    /// let first = chain.next().unwrap();   // wait=None,   trigger=Some
    /// let middle = chain.next().unwrap();  // wait=Some,   trigger=Some
    /// let last = chain.next().unwrap();    // wait=Some,   trigger=None
    /// ```
    pub fn chain(n: usize) -> Vec<Link> {
        if n == 0 {
            return Vec::new();
        }
        // n-1 channels: channel i lets link i's trigger release link (i+1)'s wait.
        let mut waits: Vec<Option<DropWait>> = Vec::with_capacity(n - 1);
        let mut triggers: Vec<Option<DropTrigger>> = Vec::with_capacity(n - 1);
        for _ in 0..n.saturating_sub(1) {
            let (w, t) = setup();
            waits.push(Some(w));
            triggers.push(Some(t));
        }

        (0..n)
            .map(|i| Link {
                wait: if i == 0 { None } else { waits[i - 1].take() },
                trigger: if i == n - 1 { None } else { triggers[i].take() },
            })
            .collect()
    }
}
