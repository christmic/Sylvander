//! Runtime-owned identity and lifecycle of concrete Agent instances.

mod model;

pub use model::{
    AgentDefinitionKey, AgentInstance, AgentInstanceOrigin, AgentInstanceState, ApprovalRoute,
    HistoryView, SessionAgentRole,
};
