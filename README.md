# eulogy

Give your data a good send off.

`Drop` lets you cleanup data, but that cleanup can't wait. It's synchronous,
meaning cleanup that takes time (making network requests, performing
file IO) will block things up. If you're writing async code, blocking calls are
the enemy. They pause your runtime, blocking other tasks on the same worker, 
driving high tail latencies and confusing to debug delays. 

`eulogy` fixes that by providing a userspace lib to implement `AsyncDrop` 
in stable rust.


## Quick start

```toml
# Cargo.toml
eulogy = { version = "0.1", features = ["tokio"] }
```

```rust,no_run
use eulogy::AsyncDrop;

struct Connection { id: u64 }

impl AsyncDrop for Connection {
    async fn async_drop(self) {
        // self.shutdown().await;
        println!("connection {} closed", self.id);
    }
}

#[tokio::main]
async fn main() {
    let conn = Connection { id: 1 }.later();
    println!("using connection {}", conn.id);
    // conn drops here and your async drop runs
}
```

`.later()` spawns a drop task and returns a drop guard. On drop, the guard sends 
your struct to the dropper where the async cleanup runs.


## Deriving `AsyncDrop`

For types whose fields all implement `AsyncDrop`, the derive macro simplifies
filling out the extra steps. Use `#[eulogy(after = [...])]` to control the
order fields are cleaned up. `after` is syntactic sugar for `eulogy::ordering`, 
look at that module to see how the primitive works.

```rust,no_run,ignore
use eulogy::AsyncDrop;

struct Socket { id: u64 }
impl AsyncDrop for Socket {
    async fn async_drop(self) { println!("socket {} closed", self.id); }
}

struct Logger { name: String }
impl AsyncDrop for Logger {
    async fn async_drop(self) { println!("logger {} flushed", self.name); }
}

#[derive(AsyncDrop)]
struct Connection {
    socket: Socket,
    // logger won't start flushing until socket has fully closed
    #[eulogy(after = [socket])]
    logger: Logger,
}

#[tokio::main]
async fn main() {
    let _conn = Connection {
        socket: Socket { id: 1 },
        logger: Logger { name: "audit".into() },
    }
    .later();
    // drop order: socket first, then logger
}
```

Fields that don't need async cleanup can be opted out with `#[eulogy(skip)]`.
For more details, see `./examples`.

## Installation

Pick the runtime that matches your application:

```toml
# tokio
eulogy = { version = "0.1", features = ["tokio"] }

# smol
eulogy = { version = "0.1", features = ["smol"] }
```

To derive `AsyncDrop` on structs and enums, add the `derive` feature:

```toml
eulogy = { version = "0.1", features = ["tokio", "derive"] }
```

Only one of `tokio` / `smol` can be active in a build at a time.

### Using eulogy in a library

Runtime agnostic library code can implement `AsyncDrop` and call `.later()`
without picking a runtime. This lets you leave runtime choice up to your
caller.

```rust,no_run
use eulogy::{AsyncDrop, DropLater};

pub struct Session { /* ... */ }

impl AsyncDrop for Session {
    async fn async_drop(self) { /* tear down */ }
}

impl Session {
    pub fn open() -> DropLater<Self> {
        Session { /* ... */ }.later()
    }
}
```

For library tests, where `.later()` needs a runtime, you have a few options:

**1. Runtime as a dev-dependency**. Your published crate stays
runtime-agnostic; only your tests see tokio:

```toml
[dependencies]
eulogy = "0.1"

[dev-dependencies]
eulogy = { version = "0.1", features = ["tokio"] }
```

**2. Passthrough features** — let downstream crates flip the runtime via
your crate's feature flags:

```toml
[features]
tokio = ["eulogy/tokio"]
smol  = ["eulogy/smol"]
```

**3. Accept a `Spawner`** — use `later_with` instead of `.later()`. No
feature flags needed at all, and tests can pass a custom spawner:

```rust,no_run
use eulogy::{AsyncDrop, DropLater, Spawner};

struct Session { /* ... */ }

impl AsyncDrop for Session {
    async fn async_drop(self) { /* tear down */ }
}

impl Session {
    pub fn open_with(spawner: &impl Spawner) -> DropLater<Self> {
        Session { /* ... */ }.later_with(spawner)
    }
}
```

A full worked example for a runtime-agnostic library app lives in
[`examples/library-poc`](examples/library-poc/README.md).

## Why `eulogy`?

`AsyncDrop` isn't yet standardised for the Rust standard library, so we need 
a way to fill the gap. There are a few alternatives, but `eulogy` aims to be
the most ergonomic, still performant, and to cover the most use cases.

### Why not `async-dropper`?

`async-dropper` provides two approaches. The `simple` module wraps your type in
`AsyncDropper<T>` and provides `async_drop(&mut self)`. The `derive` module
requires `T: Default + PartialEq` so it can compare a reset instance against
the default to determine whether to drop. It also keeps a `Mutex<T::default()>`
alive for the lifetime of the program and blocks the synchronous `Drop` on a
timeout while it waits for the async future.

`eulogy` takes a slightly different approach:

- **Simpler bounds.** `async_drop(self)` takes ownership, so there's no
  need for `Default` or `PartialEq`. Any type that implements `Send` works.
- **Consuming cleanup.** Because you own the value, you can move it into an
  async function, close handles, send on channels, or do anything else that
  requires ownership.
- **No blocking in `Drop`.** Instead of waiting on a timeout, eulogy sends
  the value through a channel and returns immediately. Cleanup runs on the
  existing runtime without holding up the caller.
- **Ordering primitives.** The `ordering` module and `#[eulogy(after = [...])]`
  give you explicit control over cleanup sequencing, which `async-dropper`
  has no equivalent for.

### Why not just block in Drop?

`block_on` inside `Drop` works until it doesn't: it panics if called from
within an async context (which is exactly where you're dropping most async
resources). You'd need to detect the context, spawn a blocking thread as a
fallback, and handle the case where no runtime exists at all. Eulogy does that
bookkeeping once so you don't have to.

## Examples

```sh
# Parent/child directory cleanup with ordered drop — tokio
cargo run --example tokio --features tokio

# Same example on smol
cargo run --example smol --features smol

# Derive with after= ordering
cargo run --example derive --features derive,tokio

# Derive on an enum
cargo run --example derive_enum --features derive,tokio
```

## Feature flags

| Flag     | Description                                         |
|----------|-----------------------------------------------------|
| `tokio`  | Enable `.later()` backed by `tokio::spawn`          |
| `smol`   | Enable `.later()` backed by `smol::spawn`           |
| `derive` | Enable `#[derive(AsyncDrop)]`                       |
