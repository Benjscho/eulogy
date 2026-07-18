//! Example: `#[derive(AsyncDrop)]` on an enum, with per-variant field shapes —
//! named fields with ordering, a tuple variant, a skipped field, and a unit
//! variant that's a no-op.

use eulogy::AsyncDrop;

#[derive(Debug)]
struct Socket {
    label: String,
}

impl AsyncDrop for Socket {
    async fn async_drop(self) {
        println!("[closing socket] {}", self.label);
    }
}

#[derive(Debug)]
struct Logger {
    label: String,
}

impl AsyncDrop for Logger {
    async fn async_drop(self) {
        println!("[flushing logger] {}", self.label);
    }
}

/// A tag identifying which pool a connection came from. Not a resource —
/// dropped synchronously, so it's `#[eulogy(skip)]`.
#[derive(Debug)]
struct PoolTag(#[allow(dead_code)] &'static str);

/// A connection in one of several states, each with its own field shape.
#[derive(Debug, AsyncDrop)]
enum Connection {
    /// Logger is flushed only after the socket finishes closing.
    Tcp {
        sock: Socket,
        #[eulogy(after = [sock])]
        logger: Logger,
    },
    /// A single field, referenced by position.
    Unix(Socket),
    /// The pool tag is sync-only and skipped.
    Pooled(
        Socket,
        #[eulogy(skip)]
        #[allow(dead_code)]
        PoolTag,
    ),
    /// No fields to drop — this arm is a no-op.
    Closed,
}

#[tokio::main]
async fn main() {
    let tcp = Connection::Tcp {
        sock: Socket {
            label: "tcp-1".into(),
        },
        logger: Logger {
            label: "tcp-1-log".into(),
        },
    }
    .later();

    let unix = Connection::Unix(Socket {
        label: "unix-1".into(),
    })
    .later();

    let pooled = Connection::Pooled(
        Socket {
            label: "pooled-1".into(),
        },
        PoolTag("default"),
    )
    .later();

    let closed = Connection::Closed.later();

    drop(tcp);
    drop(unix);
    drop(pooled);
    drop(closed);

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
}
