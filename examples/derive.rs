//! Example: using #[derive(AsyncDrop)] with ordering via `after`.

use std::path::PathBuf;

use eulogy::{later, AsyncDrop};

#[derive(Debug)]
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    async fn create(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        tokio::fs::create_dir_all(&path).await.unwrap();
        println!("[created] {}", path.display());
        Self { path }
    }
}

impl AsyncDrop for TempDir {
    async fn async_drop(self) {
        println!("[removing] {}", self.path.display());
        tokio::fs::remove_dir_all(&self.path).await.unwrap();
        println!("[removed] {}", self.path.display());
    }
}

/// Child is dropped before parent because parent declares `after = [child]`.
#[derive(Debug, AsyncDrop)]
struct Deployment {
    child: TempDir,

    #[eulogy(after = [child])]
    parent: TempDir,
}

#[tokio::main]
async fn main() {
    let parent = TempDir::create("/tmp/eulogy-derive-example").await;
    let child = TempDir::create("/tmp/eulogy-derive-example/subdir").await;

    let deployment = later(Deployment { child, parent });

    println!("using: {}, {}", deployment.child.path.display(), deployment.parent.path.display());

    drop(deployment);

    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
}
