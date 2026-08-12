//! Durable storage owned by the Runtime.
//!
//! Storage records lifecycle state and artifacts produced around Agent
//! execution. The Agent kernel receives an immutable conversation snapshot and
//! returns an outcome; it neither selects a backend nor persists records.

use std::sync::Arc;

use sylvander_agent::tools::MemoryStore;

use self::session::SessionStore;

/// Closed composition root for Runtime-owned durable repositories.
///
/// This type exists to prevent the top-level Runtime and its consumers from
/// exposing concrete repositories as independent application services. It is
/// deliberately crate-private: storage backend selection is a product
/// decision made during Runtime boot, not a public plugin contract.
pub(crate) struct RuntimeStorage {
    sessions: Arc<dyn SessionStore>,
    // Runtime retains ownership even though configured Agent revisions consume
    // cloned handles for normal reads and writes.
    #[allow(dead_code)]
    memory: Arc<dyn MemoryStore>,
}

impl RuntimeStorage {
    /// Freeze the repositories selected by the Runtime composition root.
    pub(crate) fn new(sessions: Arc<dyn SessionStore>, memory: Arc<dyn MemoryStore>) -> Self {
        Self { sessions, memory }
    }

    /// Access Session persistence inside Runtime-owned application services.
    pub(crate) fn sessions(&self) -> &Arc<dyn SessionStore> {
        &self.sessions
    }

    /// Access relationship memory inside Runtime-owned composition.
    #[cfg(test)]
    pub(crate) fn memory(&self) -> &Arc<dyn MemoryStore> {
        &self.memory
    }
}

/// Durable relationship-memory backend, integrity, backup, and maintenance.
///
/// This is a closed Runtime implementation, not a storage plugin boundary.
#[allow(dead_code)]
// operator recovery wiring is composed through Runtime services in a later batch
pub(crate) mod memory;
/// Session metadata, transcript, usage, and turn-snapshot persistence.
pub mod session;
/// Filesystem adapter for oversized tool-result artifacts.
#[allow(dead_code)]
// wired when Runtime compression policy gains an explicit artifact root
pub(crate) mod tool_result_disk;
/// Filesystem-backed workspace mutation journal and rollback recovery.
pub(crate) mod workspace_journal;
