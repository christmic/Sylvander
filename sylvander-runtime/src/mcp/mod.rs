//! Session-owned Model Context Protocol runtime.
//!
//! This module owns authenticated Session binding, connection generations,
//! transport clients, persistent process environments, and neutral tool
//! snapshots. Agent definitions remain declarative and Agent code never owns
//! MCP protocol or process authority.

mod session;

pub(crate) use session::{SessionMcpBinding, SessionMcpRuntimeService};
