//! External benchmark adapters and interoperable Agent trajectory evidence.
//!
//! Production Agent and Runtime crates own execution behavior. This crate
//! consumes their public events for cross-task evaluation and is never a
//! production dependency.

pub mod atif;

pub use atif::{
    Agent, FinalMetrics, Metrics, Observation, ObservationResult, Source, Step, ToolCall,
    Trajectory,
};
