//! Tests modeled after real-world async drop use cases from the RCN thread.

#![cfg(feature = "tokio")]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use eulogy::{later, AsyncDrop};
use tokio::sync::Mutex;

// =============================================================================
// Use case 1: Flush-on-drop
//
// A batching writer that accumulates records and flushes periodically.
// On drop, it must perform a final async flush to avoid data loss.
// =============================================================================

#[derive(Debug)]
struct BatchWriter {
    buffer: Vec<String>,
    sink: Arc<Mutex<Vec<String>>>,
}

impl BatchWriter {
    fn new(sink: Arc<Mutex<Vec<String>>>) -> Self {
        Self { buffer: Vec::new(), sink }
    }

    fn write(&mut self, record: String) {
        self.buffer.push(record);
    }
}

impl AsyncDrop for BatchWriter {
    async fn async_drop(self) {
        // Final flush.
        if !self.buffer.is_empty() {
            let mut sink = self.sink.lock().await;
            sink.extend(self.buffer);
        }
    }
}

#[tokio::test]
async fn flush_on_drop_no_data_loss() {
    let sink = Arc::new(Mutex::new(Vec::new()));

    let mut writer = later(BatchWriter::new(sink.clone()));
    writer.write("record-1".into());
    writer.write("record-2".into());
    writer.write("record-3".into());

    // Drop without explicit flush — async_drop handles it.
    drop(writer);

    tokio::time::sleep(Duration::from_millis(50)).await;

    let flushed = sink.lock().await;
    assert_eq!(*flushed, vec!["record-1", "record-2", "record-3"]);
}

// =============================================================================
// Use case 2: Connection recovery spawn
//
// A database connection tied to a microVM. If the connection drops mid-
// transaction, we attempt async recovery of the VM before giving up.
// =============================================================================

#[derive(Debug)]
struct MicroVm {
    recovered: Arc<AtomicBool>,
}

impl MicroVm {
    async fn recover(&self) -> bool {
        // Simulate recovery attempt.
        tokio::time::sleep(Duration::from_millis(20)).await;
        self.recovered.store(true, Ordering::SeqCst);
        true
    }
}

#[derive(Debug)]
struct Connection {
    vm: Option<MicroVm>,
    transaction_complete: bool,
}

impl AsyncDrop for Connection {
    async fn async_drop(mut self) {
        if let Some(vm) = self.vm.take() {
            if !self.transaction_complete {
                // Attempt recovery instead of just destroying.
                vm.recover().await;
            }
            // VM is dropped (returned to pool in real code).
        }
    }
}

#[tokio::test]
async fn connection_recovery_on_incomplete_transaction() {
    let recovered = Arc::new(AtomicBool::new(false));

    let conn = later(Connection {
        vm: Some(MicroVm { recovered: recovered.clone() }),
        transaction_complete: false,
    });

    // Simulate premature close.
    drop(conn);

    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(recovered.load(Ordering::SeqCst), "VM should have been recovered");
}

#[tokio::test]
async fn connection_clean_close_no_recovery() {
    let recovered = Arc::new(AtomicBool::new(false));

    let mut conn = later(Connection {
        vm: Some(MicroVm { recovered: recovered.clone() }),
        transaction_complete: false,
    });

    // Mark transaction complete (would normally return VM to pool here).
    conn.transaction_complete = true;
    drop(conn);

    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!recovered.load(Ordering::SeqCst), "no recovery needed");
}

// =============================================================================
// Use case 3: SSH disconnect
//
// An SSH session that needs to send an async disconnect command on drop,
// replacing the problematic `block_in_place` + `block_on` workaround.
// =============================================================================

#[derive(Debug)]
struct SshSession {
    connected: Arc<AtomicBool>,
    disconnect_called: Arc<AtomicBool>,
}

impl SshSession {
    fn new() -> (Self, Arc<AtomicBool>, Arc<AtomicBool>) {
        let connected = Arc::new(AtomicBool::new(true));
        let disconnect_called = Arc::new(AtomicBool::new(false));
        (
            Self { connected: connected.clone(), disconnect_called: disconnect_called.clone() },
            connected,
            disconnect_called,
        )
    }

    async fn disconnect(&self) {
        // Simulate async network disconnect handshake.
        tokio::time::sleep(Duration::from_millis(10)).await;
        self.connected.store(false, Ordering::SeqCst);
        self.disconnect_called.store(true, Ordering::SeqCst);
    }
}

#[derive(Debug)]
struct SshBinary {
    session: Option<SshSession>,
}

impl AsyncDrop for SshBinary {
    async fn async_drop(mut self) {
        if let Some(session) = self.session.take() {
            session.disconnect().await;
        }
    }
}

#[tokio::test]
async fn ssh_disconnect_on_drop() {
    let (session, connected, disconnect_called) = SshSession::new();

    let binary = later(SshBinary { session: Some(session) });

    assert!(connected.load(Ordering::SeqCst));
    assert!(!disconnect_called.load(Ordering::SeqCst));

    drop(binary);

    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!connected.load(Ordering::SeqCst), "should be disconnected");
    assert!(disconnect_called.load(Ordering::SeqCst), "disconnect was called");
}

#[tokio::test]
async fn ssh_already_disconnected() {
    let (session, _connected, disconnect_called) = SshSession::new();
    // Simulate already disconnected.
    session.connected.store(false, Ordering::SeqCst);

    let binary = later(SshBinary { session: Some(session) });
    drop(binary);

    tokio::time::sleep(Duration::from_millis(50)).await;
    // disconnect() still called (it's idempotent in our impl).
    assert!(disconnect_called.load(Ordering::SeqCst));
}

// =============================================================================
// Use case 4: Database transaction
//
// A transaction that must send COMMIT or ROLLBACK over async IO before the
// connection is reused. Without async drop, forgetting to finalize leaves
// the connection in a broken state.
// =============================================================================

#[derive(Debug, Clone, PartialEq)]
enum TxCommand {
    Begin,
    Commit,
    Rollback,
}

#[derive(Debug)]
struct FakeConnection {
    commands: Arc<Mutex<Vec<TxCommand>>>,
}

impl FakeConnection {
    async fn send(&self, cmd: TxCommand) {
        // Simulate async network IO.
        tokio::time::sleep(Duration::from_millis(5)).await;
        self.commands.lock().await.push(cmd);
    }
}

#[derive(Debug)]
struct Transaction {
    conn: Arc<FakeConnection>,
    committed: bool,
}

impl Transaction {
    async fn begin(conn: Arc<FakeConnection>) -> Self {
        conn.send(TxCommand::Begin).await;
        Self { conn, committed: false }
    }

    async fn commit(&mut self) {
        self.conn.send(TxCommand::Commit).await;
        self.committed = true;
    }
}

impl AsyncDrop for Transaction {
    async fn async_drop(self) {
        if !self.committed {
            // Safety net: ROLLBACK if not explicitly committed.
            self.conn.send(TxCommand::Rollback).await;
        }
    }
}

#[tokio::test]
async fn transaction_rollback_on_drop() {
    let commands = Arc::new(Mutex::new(Vec::new()));
    let conn = Arc::new(FakeConnection { commands: commands.clone() });

    let tx = later(Transaction::begin(conn.clone()).await);

    // Simulate: user forgets to commit, drops the transaction.
    drop(tx);

    tokio::time::sleep(Duration::from_millis(50)).await;

    let cmds = commands.lock().await;
    assert_eq!(*cmds, vec![TxCommand::Begin, TxCommand::Rollback]);
}

#[tokio::test]
async fn transaction_no_rollback_after_commit() {
    let commands = Arc::new(Mutex::new(Vec::new()));
    let conn = Arc::new(FakeConnection { commands: commands.clone() });

    let mut tx = later(Transaction::begin(conn.clone()).await);
    tx.commit().await;
    drop(tx);

    tokio::time::sleep(Duration::from_millis(50)).await;

    let cmds = commands.lock().await;
    assert_eq!(*cmds, vec![TxCommand::Begin, TxCommand::Commit]);
}
