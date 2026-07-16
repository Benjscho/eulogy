//! Example: async cleanup of temp directories with ordered drop using smol.

use std::path::PathBuf;

use eulogy::{ordering, AsyncDrop};

#[derive(Debug)]
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    async fn create(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        smol::fs::create_dir_all(&path).await.unwrap();
        println!("[created] {}", path.display());
        Self { path }
    }
}

impl AsyncDrop for TempDir {
    async fn async_drop(self) {
        println!("[removing] {}", self.path.display());
        smol::fs::remove_dir_all(&self.path).await.unwrap();
        println!("[removed] {}", self.path.display());
    }
}

#[derive(Debug)]
struct Parent {
    dir: TempDir,
    wait: ordering::DropWait,
}

impl AsyncDrop for Parent {
    async fn async_drop(self) {
        self.wait.wait().await;
        self.dir.async_drop().await;
    }
}

#[derive(Debug)]
struct Child {
    dir: TempDir,
    _trigger: ordering::DropTrigger,
}

impl AsyncDrop for Child {
    async fn async_drop(self) {
        self.dir.async_drop().await;
    }
}

fn main() {
    smol::block_on(async {
        let (wait, trigger) = ordering::setup();

        let parent = Parent {
            dir: TempDir::create("/tmp/eulogy-smol-example").await,
            wait,
        }
        .later();

        let child = Child {
            dir: TempDir::create("/tmp/eulogy-smol-example/subdir").await,
            _trigger: trigger,
        }
        .later();

        println!(
            "using dirs: {}, {}",
            parent.dir.path.display(),
            child.dir.path.display()
        );

        drop(child);
        drop(parent);

        smol::Timer::after(std::time::Duration::from_millis(200)).await;
    });
}
