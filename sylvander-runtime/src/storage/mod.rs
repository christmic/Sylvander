//! Durable storage owned by the Runtime.
//!
//! Storage records lifecycle state and artifacts produced around Agent
//! execution. The Agent kernel receives an immutable conversation snapshot and
//! returns an outcome; it neither selects a backend nor persists records.

/// Durable relationship-memory backend, integrity, backup, and maintenance.
///
/// This is a closed Runtime implementation, not a storage plugin boundary.
#[allow(dead_code)]
// operator recovery wiring is composed through Runtime services in a later batch
pub(crate) mod memory;
/// Session metadata, transcript, usage, and turn-snapshot persistence.
pub mod session;
