//! Provider-neutral execution policy and model/tool iteration state machine.

pub mod agent_loop;

pub use agent_loop::{AgentLoop, AgentLoopBuilder, run, run_stream, run_with_events};
