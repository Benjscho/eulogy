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
use std::{fmt, ops};

/// A type that requires async cleanup.
pub trait AsyncDrop: Send + Sync + fmt::Debug + 'static {
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
#[must_use]
#[derive(Debug)]
pub struct DropLater<T: AsyncDrop> {
    value: Option<T>,
    dropper: Option<async_channel::Sender<T>>,
}

impl<T: AsyncDrop> DropLater<T> {
    fn new(value: T, dropper: async_channel::Sender<T>) -> Self {
        Self {
            value: Some(value),
            dropper: Some(dropper),
        }
    }
}

impl<T: AsyncDrop> Drop for DropLater<T> {
    fn drop(&mut self) {
        let value = self.value.take().expect("drop runs once");
        let sender = self.dropper.take().expect("drop runs once");
        if sender.try_send(value).is_err() {
            tracing::trace!("leaking resource (drop task canceled)");
        }
    }
}

impl<T: AsyncDrop> ops::Deref for DropLater<T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        self.value.as_ref().unwrap()
    }
}

impl<T: AsyncDrop> ops::DerefMut for DropLater<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.value.as_mut().unwrap()
    }
}

/// Wrap a value so its [`AsyncDrop`] runs when the guard is dropped.
///
/// Uses the runtime enabled via feature flag (`tokio` or `smol`).
/// Libraries call this without caring which runtime the binary chose.
///
/// # Panics
///
/// Compile-time error if no runtime feature is enabled.
#[cfg(feature = "tokio")]
pub fn later<T: AsyncDrop>(value: T) -> DropLater<T> {
    later_with(value, &TokioSpawner)
}

/// Wrap a value so its [`AsyncDrop`] runs when the guard is dropped.
///
/// Uses the runtime enabled via feature flag (`tokio` or `smol`).
/// Libraries call this without caring which runtime the binary chose.
///
/// # Panics
///
/// Compile-time error if no runtime feature is enabled.
#[cfg(all(feature = "smol", not(feature = "tokio")))]
pub fn later<T: AsyncDrop>(value: T) -> DropLater<T> {
    later_with(value, &SmolSpawner)
}

/// Wrap a value so its [`AsyncDrop`] runs when the guard is dropped,
/// using a custom [`Spawner`].
pub fn later_with<T: AsyncDrop>(value: T, spawner: &impl Spawner) -> DropLater<T> {
    let (tx, rx) = async_channel::bounded(1);
    let guard = DropLater::new(value, tx);
    spawner.spawn(Box::pin(async move {
        if let Ok(value) = rx.recv().await {
            value.async_drop().await;
        }
    }));
    guard
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
}
