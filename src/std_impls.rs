//! `AsyncDrop` impls for standard library types that cannot structurally
//! hold an async resource — primitive scalars, `String`, path/time value
//! types, and forwarding impls for `Option`, `Box`, `Vec`, and tuples.
//! These exist purely for derive ergonomics, so a field of one of these
//! types doesn't need `#[eulogy(skip)]`.
//!
//! Deliberately **not** covered, since their drop semantics are
//! ambiguous or out of scope for this pass:
//! - Shared-ownership / interior-mutability wrappers: `Arc`, `Rc`,
//!   `RefCell`, `Mutex`, `RwLock` — it's unclear who should be
//!   responsible for running cleanup when ownership is shared.
//! - Keyed collections: `HashMap`, `BTreeMap`, `HashSet`, `BTreeSet`.
//! - Fixed-size arrays (`[T; N]`) and reference types (`&T`, `&str`).

use crate::AsyncDrop;

macro_rules! impl_noop_async_drop {
    ($($ty:ty),* $(,)?) => {
        $(
            impl AsyncDrop for $ty {
                async fn async_drop(self) {}
            }
        )*
    };
}

impl_noop_async_drop!(
    i8, i16, i32, i64, i128, isize,
    u8, u16, u32, u64, u128, usize,
    f32, f64,
    bool, char, (),
    String,
    std::path::PathBuf,
    std::time::Duration, std::time::Instant, std::time::SystemTime,
);

impl<T: AsyncDrop> AsyncDrop for Option<T> {
    async fn async_drop(self) {
        if let Some(v) = self {
            v.async_drop().await;
        }
    }
}

impl<T: AsyncDrop> AsyncDrop for Box<T> {
    async fn async_drop(self) {
        (*self).async_drop().await;
    }
}

impl<T: AsyncDrop> AsyncDrop for Vec<T> {
    async fn async_drop(self) {
        crate::__private::join_all(self.into_iter().map(|v| v.async_drop()).collect()).await;
    }
}

macro_rules! impl_tuple_async_drop {
    ($($idx:tt => $ty:ident),+) => {
        impl<$($ty: AsyncDrop),+> AsyncDrop for ($($ty,)+) {
            async fn async_drop(self) {
                $(self.$idx.async_drop().await;)+
            }
        }
    };
}

impl_tuple_async_drop!(0 => T1);
impl_tuple_async_drop!(0 => T1, 1 => T2);
impl_tuple_async_drop!(0 => T1, 1 => T2, 2 => T3);
impl_tuple_async_drop!(0 => T1, 1 => T2, 2 => T3, 3 => T4);
impl_tuple_async_drop!(0 => T1, 1 => T2, 2 => T3, 3 => T4, 4 => T5);
impl_tuple_async_drop!(0 => T1, 1 => T2, 2 => T3, 3 => T4, 4 => T5, 5 => T6);
impl_tuple_async_drop!(0 => T1, 1 => T2, 2 => T3, 3 => T4, 4 => T5, 5 => T6, 6 => T7);
impl_tuple_async_drop!(0 => T1, 1 => T2, 2 => T3, 3 => T4, 4 => T5, 5 => T6, 6 => T7, 7 => T8);
impl_tuple_async_drop!(0 => T1, 1 => T2, 2 => T3, 3 => T4, 4 => T5, 5 => T6, 6 => T7, 7 => T8, 8 => T9);
impl_tuple_async_drop!(0 => T1, 1 => T2, 2 => T3, 3 => T4, 4 => T5, 5 => T6, 6 => T7, 7 => T8, 8 => T9, 9 => T10);
impl_tuple_async_drop!(0 => T1, 1 => T2, 2 => T3, 3 => T4, 4 => T5, 5 => T6, 6 => T7, 7 => T8, 8 => T9, 9 => T10, 10 => T11);
impl_tuple_async_drop!(0 => T1, 1 => T2, 2 => T3, 3 => T4, 4 => T5, 5 => T6, 6 => T7, 7 => T8, 8 => T9, 9 => T10, 10 => T11, 11 => T12);
