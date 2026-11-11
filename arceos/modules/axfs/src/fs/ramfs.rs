//! Mount the repository's RAMFS implementation, including same-directory rename.
//! Reuse its nodes with the kernel mutex so no dependency changes are needed.
use alloc::sync::Arc;
use axfs_vfs::{VfsNodeRef, VfsOps, VfsResult};
use axsync::Mutex;

#[path = "../../../../axfs_ramfs/src/dir.rs"]
mod dir;
#[path = "../../../../axfs_ramfs/src/file.rs"]
mod file;

pub struct RamFileSystem {
    root: Arc<dir::DirNode>,
}

impl RamFileSystem {
    pub fn new() -> Self {
        Self {
            root: dir::DirNode::new(None),
        }
    }
}

impl VfsOps for RamFileSystem {
    fn mount(&self, _path: &str, mount_point: VfsNodeRef) -> VfsResult {
        // The parent remains owned by the root filesystem for this mount's life.
        self.root.set_parent(mount_point.parent().as_ref());
        Ok(())
    }
    fn root_dir(&self) -> VfsNodeRef {
        self.root.clone()
    }
}
