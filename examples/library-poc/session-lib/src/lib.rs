//! Toy library that hands callers an async-cleaned-up `Session`.
//!
//! This crate depends on `eulogy` with no runtime feature enabled. It's the
//! downstream binary (see `poc/tokio-app` and `poc/smol-app`) that picks a
//! runtime; Cargo feature unification makes `.later()` resolve against that
//! runtime when this library is compiled as part of the binary's build.

use eulogy::{AsyncDrop, DropLater};

pub struct Session {
    pub id: u64,
}

impl AsyncDrop for Session {
    async fn async_drop(self) {
        println!("[session-lib] closing session {}", self.id);
    }
}

impl Session {
    pub fn open(id: u64) -> DropLater<Self> {
        println!("[session-lib] opening session {id}");
        Session { id }.later()
    }
}
