//! # Eulogy
//!
//! Give your data a good send off.
//!
//! Userspace async drop for Rust. Provides an [`AsyncDrop`] trait and a
//! [`DropLater`] guard that runs async cleanup when a value is dropped.
//!
//! ## Usage
//!
//! Enable the runtime feature that matches your app:
//!
//! ```toml
//! eulogy = { version = "0.1", features = ["tokio"] }
//! ```
//!
//! Implement [`AsyncDrop`] for your type, then call [`AsyncDrop::later`] to
//! get a guard that runs cleanup when it's dropped:
//!
//! ```
//! # #[cfg(all(feature = "tokio", not(feature = "smol")))] {
//! use eulogy::AsyncDrop;
//!
//! struct Connection {
//!     id: u64,
//! }
//!
//! impl AsyncDrop for Connection {
//!     async fn async_drop(self) {
//!         // e.g. self.close().await;
//!         println!("closing {}", self.id);
//!     }
//! }
//!
//! # tokio::runtime::Runtime::new().unwrap().block_on(async {
//! let conn = Connection { id: 1 }.later();
//! // ...use `conn` via Deref/DerefMut...
//! drop(conn); // drops the connection async
//! # });
//! # }
//! ```
//!
//! ### Deriving `AsyncDrop`
//!
//! Instead of manually implementing on each struct, you can derive AsyncDrop
//! for any structs whose fields all implement it. You can use 
//! `#[eulogy(after = [...])]` to enforce drop ordering between fields:
//!
//! ```toml
//! eulogy = { version = "0.1", features = ["tokio", "derive"] }
//! ```
//!
//! ```
//! # #[cfg(all(feature = "tokio", not(feature = "smol"), feature = "derive"))] {
//! use eulogy::AsyncDrop;
//!
//! struct Socket { id: u64 }
//! impl AsyncDrop for Socket {
//!     async fn async_drop(self) { /* close */ }
//! }
//!
//! struct Logger { name: String }
//! impl AsyncDrop for Logger {
//!     async fn async_drop(self) { /* flush */ }
//! }
//!
//! #[derive(AsyncDrop)]
//! struct Connection {
//!     socket: Socket,
//!     // Wait for `socket` to close before flushing the logger.
//!     #[eulogy(after = [socket])]
//!     logger: Logger,
//! }
//!
//! # tokio::runtime::Runtime::new().unwrap().block_on(async {
//! let conn = Connection {
//!     socket: Socket { id: 1 },
//!     logger: Logger { name: "audit".into() },
//! }
//! .later();
//! drop(conn);
//! # });
//! # }
//! ```
//!
//! For fields that don't (or shouldn't) implement `AsyncDrop`, annotate them
//! with `#[eulogy(skip)]`.
//!
//! ### Libraries
//!
//! Library authors can use `AsyncDrop` without picking a runtime. Just
//! implement the trait and call `.later()` — the same code works whether
//! the final binary enables `tokio` or `smol`:
//!
//! ```
//! # #[cfg(all(any(feature = "tokio", feature = "smol"), not(all(feature = "tokio", feature = "smol"))))] {
//! use eulogy::{AsyncDrop, DropLater};
//!
//! pub struct Session { /* ... */ }
//!
//! impl AsyncDrop for Session {
//!     async fn async_drop(self) { /* tear down */ }
//! }
//!
//! impl Session {
//!     pub fn open() -> DropLater<Self> {
//!         Session { /* ... */ }.later()
//!     }
//! }
//! # }
//! ```
//!
//! Cargo unifies features across the dep graph, so a library depending on
//! `eulogy` with **no features** still gets a working `.later()` — the binary
//! above it turns on `tokio` or `smol`, and the library's call resolves
//! against that same build. The library doesn't need its own runtime flags
//! for its consumers.
//!
//! Library devs do need to select a runtime for any testing. With no runtime
//! feature, `.later()` won't compile. To handle this you can do one of the 
//! following:
//!
//! 1. **Turn on a runtime as a dev-dependency.** Simplest for most libraries
//!    — `.later()` is available in tests, but consumers still pick their own
//!    runtime:
//!
//!    ```toml
//!    [dependencies]
//!    eulogy = "0.1"
//!
//!    [dev-dependencies]
//!    eulogy = { version = "0.1", features = ["tokio"] }
//!    ```
//!
//! 2. **Passthrough features.** Re-expose the runtime choice so downstream
//!    can flip it via your crate's features:
//!
//!    ```toml
//!    [features]
//!    tokio = ["eulogy/tokio"]
//!    smol = ["eulogy/smol"]
//!    ```
//!
//! 3. **Accept a [`Spawner`] from the caller.** Use [`AsyncDrop::later_with`]
//!    instead of `.later()` — no feature flags needed, and callers can plug
//!    in whatever runtime they use (including a custom test spawner):
//!
//!    ```
//!    use eulogy::{AsyncDrop, DropLater, Spawner};
//!
//!    # struct Session;
//!    # impl AsyncDrop for Session { async fn async_drop(self) {} }
//!    impl Session {
//!        pub fn open_with(spawner: &impl Spawner) -> DropLater<Self> {
//!            Session { /* ... */ }.later_with(spawner)
//!        }
//!    }
//!    ```
//!
//! ## Ordering
//!
//! The [`ordering`] module provides primitives to enforce drop order between
//! related resources (e.g. a parent directory must outlive its children).

// Let `::eulogy` resolve inside the crate too, so the `AsyncDrop` derive can
// emit fully-qualified paths that work both here and in downstream crates.
extern crate self as eulogy;

use std::future::Future;
use std::pin::Pin;
use std::ops;

#[cfg(all(feature = "tokio", feature = "smol"))]
compile_error!(
    "eulogy: enable only one of `tokio` or `smol` \
     If a transitive dep is enabling the other feature, disable its \
     default features."
);

/// Re-export the derive macro when the `derive` feature is enabled.
#[cfg(feature = "derive")]
pub use eulogy_derive::AsyncDrop;

mod std_impls;

/// A type that can perform async cleanup.
pub trait AsyncDrop: Send {
    /// Perform async cleanup, consuming the value.
    fn async_drop(self) -> impl Future<Output = ()> + Send;

    /// Wrap `self` in a guard that runs [`async_drop`](Self::async_drop) when
    /// the guard is dropped. Uses the runtime enabled via feature flag.
    ///
    /// # Panics
    ///
    /// Must be called from within a runtime context.
    #[cfg(all(feature = "tokio", not(feature = "smol")))]
    fn later(self) -> DropLater<Self>
    where
        Self: Sized + 'static,
    {
        self.later_with(&TokioSpawner)
    }

    /// Wrap `self` in a guard that runs [`async_drop`](Self::async_drop) when
    /// the guard is dropped. Uses the runtime enabled via feature flag.
    ///
    /// # Panics
    ///
    /// Must be called from within a `smol` scope.
    #[cfg(all(feature = "smol", not(feature = "tokio")))]
    fn later(self) -> DropLater<Self>
    where
        Self: Sized + 'static,
    {
        self.later_with(&SmolSpawner)
    }

    /// Wrap `self` in a guard using a custom [`Spawner`].
    fn later_with(self, spawner: &impl Spawner) -> DropLater<Self>
    where
        Self: Sized + 'static,
    {
        let (tx, rx) = async_channel::bounded(1);
        let guard = DropLater::new(self, tx);
        spawner.spawn(Box::pin(async move {
            if let Ok(value) = rx.recv().await {
                value.async_drop().await;
            }
        }));
        guard
    }
}

/// Spawns a future onto an async runtime.
///
/// You can implement this to use a custom runtime with
/// [`AsyncDrop::later_with`].
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
/// let a = A { .. }.later();
/// let b = B { .. }.later();
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
        self.value.as_ref().expect("value is present until Drop")
    }
}

impl<T: AsyncDrop + 'static> ops::DerefMut for DropLater<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.value.as_mut().expect("value is present until Drop")
    }
}

impl<T: AsyncDrop + std::fmt::Debug + 'static> std::fmt::Debug for DropLater<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Don't leak the internal Option representation — print the inner T.
        f.debug_tuple("DropLater").field(&**self).finish()
    }
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
        (DropWait(rx), DropTrigger { _closer: tx })
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
    pub struct DropTrigger {
        // Held solely for its Drop impl: when the last clone drops, the channel
        // closes and the paired DropWait resolves.
        _closer: async_channel::Sender<()>,
    }
}
