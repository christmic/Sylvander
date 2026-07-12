//! Disk persistence layer for the L0 [`ToolResultBudgetLayer`].
//!
//! When a `tool_result` is too large to keep inline in the message
//! history, the L0 layer writes the full content to disk and replaces
//! the inline block with a preview + path. The model can then read
//! the full content back via a Read tool (or any other file-reading
//! mechanism) when needed.
//!
//! ## Why a trait
//!
//! Production uses [`FilesystemToolResultDisk`] (writes to
//! `std::env::temp_dir()`). Tests use [`InMemoryToolResultDisk`] to
//! avoid touching the real filesystem. A future `S3ToolResultDisk`
//! can drop in without changing the layer.
//!
//! ## Sync by design
//!
//! Disk writes are fast for the sizes we expect (a few MB). The
//! trait is sync; the layer wraps calls in its `apply` future body.
//! If we ever need true async I/O, the trait can be made async
//! without breaking the layer signature (Pin<Box<dyn Future>> is
//! already async-capable).

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Handle to content persisted to disk by a [`ToolResultDisk`].
///
/// The L0 layer embeds `path` in the rewritten `tool_result` so the
/// model can find the full content. `original_bytes` is used for
/// the heuristic `freed_tokens` accounting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskHandle {
    /// Path the content was written to (or, for in-memory, a
    /// synthetic marker like `"<in-memory>/<tool_use_id>"`).
    pub path: PathBuf,
    /// Size of the original content in bytes.
    pub original_bytes: usize,
}

/// Disk persistence for oversized tool results.
pub trait ToolResultDisk: Send + Sync {
    /// Persist `body` for later retrieval. `tool_use_id` is the
    /// `ToolUseBlock.id` that produced this result — used as the
    /// filename so the model can correlate the file with the call.
    fn persist(&self, tool_use_id: &str, body: &str) -> io::Result<DiskHandle>;
}

// =============================================================================
// FilesystemToolResultDisk — production impl.
// =============================================================================

/// Production disk: writes one file per `tool_use_id` under
/// `<root>/<tool_use_id>.txt`.
///
/// Default `root` is `std::env::temp_dir()/sylvander-tool-results/`.
/// Files are NOT cleaned up automatically; the OS temp cleaner
/// handles it. If deterministic cleanup is needed, pass an explicit
/// root and drop it.
pub struct FilesystemToolResultDisk {
    root: PathBuf,
}

impl FilesystemToolResultDisk {
    /// Create a disk rooted at the system temp dir.
    pub fn new() -> io::Result<Self> {
        let root = std::env::temp_dir().join("sylvander-tool-results");
        std::fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    /// Create a disk rooted at an explicit path. Useful for tests
    /// (pass a `tempfile::TempDir` path).
    pub fn with_root(root: PathBuf) -> io::Result<Self> {
        std::fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    /// Path where content for `tool_use_id` would be written.
    #[must_use]
    pub fn path_for(&self, tool_use_id: &str) -> PathBuf {
        self.root.join(format!("{tool_use_id}.txt"))
    }
}

impl ToolResultDisk for FilesystemToolResultDisk {
    fn persist(&self, tool_use_id: &str, body: &str) -> io::Result<DiskHandle> {
        let path = self.path_for(tool_use_id);
        std::fs::write(&path, body)?;
        Ok(DiskHandle {
            path,
            original_bytes: body.len(),
        })
    }
}

// =============================================================================
// InMemoryToolResultDisk — test impl.
// =============================================================================

/// In-memory disk for tests. Tracks writes and lets tests assert on
/// what's been persisted without touching the filesystem.
#[derive(Default, Clone)]
pub struct InMemoryToolResultDisk {
    inner: Arc<Mutex<HashMap<String, String>>>,
    write_count: Arc<Mutex<usize>>,
}

impl InMemoryToolResultDisk {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Retrieve content previously persisted for `tool_use_id`.
    #[must_use]
    pub fn get(&self, tool_use_id: &str) -> Option<String> {
        self.inner.lock().unwrap().get(tool_use_id).cloned()
    }

    /// Total number of `persist` calls.
    #[must_use]
    pub fn write_count(&self) -> usize {
        *self.write_count.lock().unwrap()
    }

    /// All `tool_use_id`s that have been persisted (sorted for stable
    /// assertions).
    #[must_use]
    pub fn ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.inner.lock().unwrap().keys().cloned().collect();
        ids.sort();
        ids
    }
}

impl ToolResultDisk for InMemoryToolResultDisk {
    fn persist(&self, tool_use_id: &str, body: &str) -> io::Result<DiskHandle> {
        self.inner
            .lock()
            .unwrap()
            .insert(tool_use_id.to_string(), body.to_string());
        *self.write_count.lock().unwrap() += 1;
        Ok(DiskHandle {
            path: PathBuf::from(format!("<in-memory>/{tool_use_id}")),
            original_bytes: body.len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filesystem_disk_writes_and_returns_handle() {
        let dir = tempfile::tempdir().expect("tempdir");
        let disk = FilesystemToolResultDisk::with_root(dir.path().to_path_buf()).expect("disk");

        let handle = disk
            .persist("toolu_abc", "hello world")
            .expect("persist should succeed");

        assert_eq!(handle.original_bytes, 11);
        assert!(handle.path.exists());

        let read_back = std::fs::read_to_string(&handle.path).expect("read back");
        assert_eq!(read_back, "hello world");
    }

    #[test]
    fn filesystem_disk_path_for_is_predictable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let disk = FilesystemToolResultDisk::with_root(dir.path().to_path_buf()).expect("disk");

        let p = disk.path_for("toolu_xyz");
        assert!(p.ends_with("toolu_xyz.txt"));
    }

    #[test]
    fn in_memory_disk_records_writes() {
        let disk = InMemoryToolResultDisk::new();

        let h1 = disk.persist("a", "alpha").expect("persist a");
        let h2 = disk.persist("b", "beta beta").expect("persist b");

        assert_eq!(h1.original_bytes, 5);
        assert_eq!(h2.original_bytes, 9);
        assert_eq!(disk.write_count(), 2);
        assert_eq!(disk.ids(), vec!["a".to_string(), "b".to_string()]);
        assert_eq!(disk.get("a").as_deref(), Some("alpha"));
        assert_eq!(disk.get("b").as_deref(), Some("beta beta"));
        assert_eq!(disk.get("missing"), None);
    }

    #[test]
    fn in_memory_disk_overwrites_on_same_id() {
        let disk = InMemoryToolResultDisk::new();
        disk.persist("dup", "first").unwrap();
        disk.persist("dup", "second").unwrap();

        assert_eq!(disk.write_count(), 2);
        assert_eq!(disk.get("dup").as_deref(), Some("second"));
    }

    #[test]
    fn trait_is_object_safe() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fs: Box<dyn ToolResultDisk> =
            Box::new(FilesystemToolResultDisk::with_root(tmp.path().to_path_buf()).unwrap());
        let mem: Box<dyn ToolResultDisk> = Box::new(InMemoryToolResultDisk::new());

        // Smoke: both impls callable through trait object.
        let _ = fs.persist("x", "y").unwrap();
        let _ = mem.persist("x", "y").unwrap();
    }
}
