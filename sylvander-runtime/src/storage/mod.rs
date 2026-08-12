//! Durable storage owned by the Runtime.
//!
//! Storage records lifecycle state and artifacts produced around Agent
//! execution. The Agent kernel receives an immutable conversation snapshot and
//! returns an outcome; it neither selects a backend nor persists records.

/// Session metadata, transcript, usage, and turn-snapshot persistence.
pub mod session;
