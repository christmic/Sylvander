//! Runtime-owned filesystem storage for oversized tool results.
//!
//! The root is always explicit and application-managed. Runtime never falls
//! back to a process-global temporary directory because lifecycle, cleanup,
//! quota, and audit policy belong to the product composition boundary.

use std::fs;
use std::io;
use std::path::PathBuf;

use sylvander_agent::compress::disk::{DiskHandle, ToolResultDisk};

const MAX_TOOL_USE_ID_BYTES: usize = 128;

/// Filesystem adapter for Agent's oversized-tool-result persistence port.
#[derive(Debug)]
pub(crate) struct FilesystemToolResultDisk {
    root: PathBuf,
}

impl FilesystemToolResultDisk {
    /// Bind the adapter to a Runtime-managed artifact root.
    pub(crate) fn new(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    fn path_for(&self, tool_use_id: &str) -> io::Result<PathBuf> {
        validate_tool_use_id(tool_use_id)?;
        Ok(self.root.join(format!("{tool_use_id}.txt")))
    }
}

impl ToolResultDisk for FilesystemToolResultDisk {
    fn persist(&self, tool_use_id: &str, body: &str) -> io::Result<DiskHandle> {
        let path = self.path_for(tool_use_id)?;
        fs::write(&path, body)?;
        Ok(DiskHandle {
            path,
            original_bytes: body.len(),
        })
    }
}

fn validate_tool_use_id(tool_use_id: &str) -> io::Result<()> {
    let valid = !tool_use_id.is_empty()
        && tool_use_id.len() <= MAX_TOOL_USE_ID_BYTES
        && tool_use_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        && !matches!(tool_use_id, "." | "..");
    if valid {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "tool use id must be 1-128 ASCII identifier characters",
        ))
    }
}

#[cfg(test)]
#[path = "../../tests/unit/tool_result_disk.rs"]
mod tests;
