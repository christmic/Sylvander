//! Persistence port for oversized L0 tool results.
//! [`ToolResultBudgetLayer`](crate::compress::layers::tool_result_budget::ToolResultBudgetLayer).
//!
//! The Agent kernel decides when content must leave the model context, but the
//! Runtime decides where and how it is retained. This module therefore exposes
//! values and a port only; it does not select a host directory or perform
//! filesystem I/O.
//!
//! ## Why a trait
//!
//! Runtime may satisfy this port with a managed filesystem, object store, or
//! another artifact backend without changing compression policy.
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

#[cfg(test)]
#[path = "../../tests/unit/compress_disk.rs"]
mod tests;
