//! Compression layer trait and per-layer report.
//!
//! A [`CompressionLayer`] is one step in a [`CompressionPipeline`](super::pipeline::CompressionPipeline).
//! Each layer inspects the conversation and either rewrites it
//! (in-place) or removes messages. After running, it returns a
//! [`LayerReport`] describing what it did.
//!
//! ## Sync vs async
//!
//! The trait uses `Pin<Box<dyn Future<Output = LayerReport> + Send + 'a>>`
//! instead of `async fn` because we store layers as `Box<dyn CompressionLayer>`
//! (trait objects). The `Pin<Box<…>>` return is object-safe and lets sync
//! layers trivially wrap their body in `Box::pin(async { … })`.
//!
//! Sync layers (L0/L1/L2) do their work before returning the future —
//! the future is a thin wrapper that yields the already-computed
//! `LayerReport`. Only L4 (LLM summary) does meaningful work inside
//! the future.
//!
//! ## Failure isolation
//!
//! A layer should NEVER panic and should NEVER return `Result::Err`.
//! On error, return a [`LayerReport`] with `failure: Some(reason)`. The
//! pipeline logs and continues to the next layer.

use std::future::Future;
use std::pin::Pin;

use serde_json::Value as JsonValue;

use crate::compress::CompressContext;
use crate::compress::error::{CompactionError, CompactionFailureCode};

/// What one compression layer did in a single pass.
///
/// The pipeline aggregates a `Vec<LayerReport>` per iteration and
/// emits it on `AgentEvent::Compressed { layers }`. Helpers
/// `total_removed`, `total_condensed`, `total_freed` operate on the
/// slice.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LayerReport {
    /// Stable identifier (e.g. `"orphan_snip"`, `"micro_compact"`).
    /// Set from `CompressionLayer::name()` so events and logs are
    /// consistent across runs.
    pub name: String,
    /// Whole messages dropped from the front of the conversation.
    pub removed_count: usize,
    /// Inner blocks (e.g. `ToolResultBlock`) rewritten in place —
    /// placeholder, preview, summary — but the message remains.
    pub condensed_count: usize,
    /// Estimated tokens saved by this layer (heuristic; sum of
    /// removed + condensed deltas divided by ~4 chars/token).
    pub freed_tokens: u32,
    /// Layer-specific extras (paths written, summary token count,
    /// etc.). Opaque to the pipeline.
    pub details: Option<JsonValue>,
    /// Non-fatal error: layer produced no work but recorded why.
    /// The pipeline logs and continues.
    pub failure: Option<String>,
    /// Stable internal classification. Wire consumers continue receiving the
    /// bounded compatibility reason above.
    pub failure_code: Option<CompactionFailureCode>,
}

impl LayerReport {
    /// Construct a "no-op" report with just the layer name set.
    #[must_use]
    pub fn noop(name: &str) -> Self {
        Self {
            name: name.to_string(),
            ..Self::default()
        }
    }

    /// Construct a failure report (zero work, error recorded).
    #[must_use]
    pub fn failed(name: &str, reason: impl Into<String>) -> Self {
        Self {
            name: name.to_string(),
            failure: Some(reason.into()),
            failure_code: Some(CompactionFailureCode::Other),
            ..Self::default()
        }
    }

    #[must_use]
    pub fn failed_with(name: &str, error: CompactionError) -> Self {
        Self {
            name: name.to_string(),
            failure: Some(error.compatibility_reason().into()),
            failure_code: Some(error.code),
            ..Self::default()
        }
    }
}

/// Sum `removed_count` across all layer reports.
#[must_use]
pub fn total_removed(layers: &[LayerReport]) -> usize {
    layers.iter().map(|l| l.removed_count).sum()
}

/// Sum `condensed_count` across all layer reports.
#[must_use]
pub fn total_condensed(layers: &[LayerReport]) -> usize {
    layers.iter().map(|l| l.condensed_count).sum()
}

/// Sum `freed_tokens` across all layer reports.
#[must_use]
pub fn total_freed(layers: &[LayerReport]) -> u32 {
    layers.iter().map(|l| l.freed_tokens).sum()
}

/// First failure message across all layer reports, if any.
#[must_use]
pub fn first_failure(layers: &[LayerReport]) -> Option<&str> {
    layers.iter().find_map(|l| l.failure.as_deref())
}

#[must_use]
pub fn first_failure_error(layers: &[LayerReport]) -> Option<CompactionError> {
    layers.iter().find_map(|layer| {
        layer.failure.as_ref().map(|_| {
            CompactionError::new(layer.failure_code.unwrap_or(CompactionFailureCode::Other))
        })
    })
}

/// One layer in a [`CompressionPipeline`](super::pipeline::CompressionPipeline).
///
/// Layers mutate the messages via [`CompressContext`] and report what
/// they did via [`LayerReport`]. Layers are run sequentially by the
/// pipeline; a layer returning a `failure` does not stop subsequent
/// layers.
pub trait CompressionLayer: Send + Sync {
    /// Stable, human-readable identifier. Used in events, logs, and
    /// the `name` field of every `LayerReport` this layer produces.
    fn name(&self) -> &'static str;

    /// Apply the layer. Must not panic; must not return `Result::Err`.
    /// On error, return a `LayerReport` with `failure: Some(_)` so the
    /// pipeline can isolate the failure and continue.
    fn apply<'a>(
        &'a self,
        ctx: &'a mut CompressContext<'_>,
    ) -> Pin<Box<dyn Future<Output = LayerReport> + Send + 'a>>;
}

#[cfg(test)]
#[path = "../../tests/unit/compress_layer.rs"]
mod tests;
