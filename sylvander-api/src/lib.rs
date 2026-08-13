//! # sylvander-api
//!
//! Wire-format protocol types for Sylvander's service boundaries.
//!
//! These types are language-neutral — they define the contract between
//! agents, channels, runtimes, and clients. All data types have `serde` and
//! `schemars::JsonSchema` derives.
//!
//! ## Multi-language
//!
//! This crate intentionally contains no asynchronous runtime, transport trait,
//! or concrete delivery implementation. Rust application ports live in their
//! owning layer; only values that cross a process or service boundary belong
//! here.
//!
//! ```bash
//! # Generate JSON Schema for TypeScript/Python/etc codegen
//! cargo run -p sylvander-api --example generate_ui_schema
//! ```

/// Versioned administrative messages for Agent definition revisions.
pub mod agent_admin;
/// Authenticated ingress context and content-safe boundary failures.
pub mod boundary;
/// Redacted execution policy, progress, and recovery DTOs.
pub mod execution;
/// Evidence-bound user feedback DTOs.
pub mod feedback;
/// Stable public Agent, Session, and User identifiers.
pub mod identity;
/// Link-code protocol for mapping trusted transport identities to users.
pub mod identity_binding;
/// Versioned, owner-bound Guardian memory confirmation protocol.
pub mod memory_confirmation;
/// Versioned message envelopes and transient Runtime event DTOs.
pub mod message;
/// Provider-qualified model catalog and public reasoning DTOs.
pub mod model;
/// UI protocol version and capability negotiation DTOs.
pub mod negotiation;
/// Redacted optional-platform capability and presentation DTOs.
pub mod platform;
/// Versioned administrative messages for provider and credential registries.
pub mod registry_admin;
/// JSON Schema generation for UI and boundary protocol types.
pub mod schema;
/// Durable Session configuration and redacted public state DTOs.
pub mod session;
/// Session-scoped context, metadata, and immutable snapshots.
pub mod session_context;
/// Client-to-server UI messages and server-facing session configuration types.
pub mod ui;
/// Versioned global user-profile protocol and privacy classifications.
pub mod user_profile;
pub mod workspace_worker;

pub use agent_admin::*;
pub use boundary::*;
pub use execution::*;
pub use feedback::*;
pub use identity::*;
pub use identity_binding::*;
pub use memory_confirmation::*;
pub use message::*;
pub use model::*;
pub use negotiation::*;
pub use platform::*;
pub use registry_admin::*;
pub use session::*;
pub use session_context::*;
pub use ui::*;
pub use user_profile::*;
pub use workspace_worker::*;
