//! Shared live-conformance result contract for Sylvander LLM adapters.
//!
//! This non-production crate owns credential-gated and fault-injected bench
//! journeys. Provider crates retain their deterministic protocol tests and do
//! not depend on this crate.

mod result;

pub use result::{BenchResult, BenchStatus, PassMetrics, RepositoryState, endpoint_origin};
