//! Shared live-conformance result contract for Sylvander LLM adapters.
//!
//! This non-production crate owns credential-gated and fault-injected bench
//! journeys. Provider crates retain their deterministic protocol tests and do
//! not depend on this crate.

mod live;
mod matrix;
mod recovery;
mod result;

pub use live::{LiveLimits, run_live_cell};
pub use matrix::{
    Applicability, BenchMatrix, BenchScenario, MatrixCell, MatrixCoordinate, ModelBinding,
    ProtocolBinding,
};
pub use recovery::{run_crash_fixture, run_process_interruption_cell};
pub use result::{
    BenchObservation, BenchResult, BenchStatus, PassMetrics, RepositoryState, endpoint_origin,
};
