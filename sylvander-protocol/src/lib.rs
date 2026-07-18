//! # sylvander-protocol
//!
//! Wire-format protocol types for Sylvander's message bus.
//!
//! These types are language-neutral — they define the contract between
//! agents, channels, and the bus. All data types have `serde` and
//! `schemars::JsonSchema` derives.
//!
//! ## Multi-language
//!
//! The `types` module contains all cross-language data definitions.
//! `bus_trait` and `in_process` are Rust-only runtime types.
//!
//! ```bash
//! # Generate JSON Schema for TypeScript/Python/etc codegen
//! cargo run -p sylvander-protocol --example generate_ui_schema
//! ```

/// Versioned administrative messages for Agent definition revisions.
pub mod agent_admin;
/// Authenticated ingress context and content-safe boundary failures.
pub mod boundary;
/// Transport-neutral message-bus contract and diagnostics.
pub mod bus_trait;
/// Link-code protocol for mapping trusted transport identities to users.
pub mod identity_binding;
/// In-process implementation of the message-bus contract.
pub mod in_process;
/// Versioned administrative messages for provider and credential registries.
pub mod registry_admin;
/// JSON Schema generation for UI and boundary protocol types.
pub mod schema;
/// Session-scoped context, metadata, and immutable snapshots.
pub mod session_context;
/// Language-neutral identities, events, and cross-boundary data types.
pub mod types;
/// Client-to-server UI messages and server-facing session configuration types.
pub mod ui;
/// Versioned global user-profile protocol and privacy classifications.
pub mod user_profile;

pub use agent_admin::*;
pub use boundary::*;
pub use bus_trait::{BusDiagnostics, BusError, MessageBus, SubscriptionFilter};
pub use identity_binding::*;
pub use in_process::InProcessMessageBus;
pub use registry_admin::*;
pub use session_context::*;
pub use types::*;
pub use ui::*;
pub use user_profile::*;
