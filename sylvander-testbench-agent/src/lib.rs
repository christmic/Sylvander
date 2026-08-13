//! External benchmark adapters and interoperable Agent trajectory evidence.
//!
//! Production Agent and Runtime crates own execution behavior. This crate
//! consumes their public events for cross-task evaluation and is never a
//! production dependency.

pub mod atif;
pub mod harbor;
pub mod matrix;
pub mod recorder;
pub mod result;
pub mod swebench;

pub use atif::{
    Agent, FinalMetrics, Metrics, Observation, ObservationResult, Source, Step, ToolCall,
    Trajectory,
};
pub use recorder::{RecorderError, TrajectoryRecorder};
