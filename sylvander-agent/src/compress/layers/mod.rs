//! Compression layer implementations.
//!
//! Each layer is one step in a [`CompressionPipeline`](super::pipeline::CompressionPipeline).
//! They run sequentially in cheap-first, expensive-last order:
//!
//! | Layer | Module | Status |
//! |---|---|---|
//! | L0 | [`tool_result_budget`](self::tool_result_budget) | shipped |
//! | L1 | [`orphan_snip`](self::orphan_snip) | pending |
//! | L2 | [`micro_compact`](self::micro_compact) | pending |
//! | L3 | [`context_collapse`](self::context_collapse) | pending (M4+) |
//! | L4 | [`auto_compact`](self::auto_compact) | pending |

pub mod auto_compact;
pub mod context_collapse;
pub mod micro_compact;
pub mod orphan_snip;
pub mod tool_result_budget;