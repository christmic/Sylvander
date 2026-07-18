//! Disk persistence layer for the L0
//! [`ToolResultBudgetLayer`](crate::compress::layers::tool_result_budget::ToolResultBudgetLayer).
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
//! `std::env::temp_dir()`). Tests provide their in-memory implementation from
//! the crate's `tests/` tree. A future object-store implementation can drop in
//! without changing the layer.
//!
//! ## Sync by design
//!
//! Disk writes are fast for the sizes we expect (a few MB). The
//! trait is sync; the layer wraps calls in its `apply` future body.
//! If we ever need true async I/O, the trait can be made async
//! without breaking the layer signature (`Pin<Box<…>>` is
//! already async-capable).

use std::io;
use std::path::PathBuf;

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

    /// Create a disk rooted at an explicit application-managed path.
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

#[cfg(test)]
#[path = "../../tests/unit/compress_disk.rs"]
mod tests;
