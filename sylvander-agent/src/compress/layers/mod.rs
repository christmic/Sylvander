//! Compression layer implementations.
//!
//! Each layer is one step in a [`CompressionPipeline`](crate::compress::pipeline::CompressionPipeline).
//! They run sequentially in cheap-first, expensive-last order:
//!
//! | Layer | Module | Status |
//! |---|---|---|
//! | L0 | [`tool_result_budget`] | shipped |
//! | L1 | [`orphan_snip`] | shipped |
//! | L2 | [`micro_compact`] | shipped |
//! | L3 | [`context_collapse`] | shipped |
//! | L4 | [`auto_compact`] | shipped |

pub mod auto_compact;
pub mod context_collapse;
pub mod micro_compact;
pub mod orphan_snip;
pub mod tool_result_budget;
