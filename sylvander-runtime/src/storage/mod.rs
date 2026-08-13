//! Durable storage owned by the Runtime.
//!
//! Storage records lifecycle state and artifacts produced around Agent
//! execution. The Agent kernel receives an immutable conversation snapshot and
//! returns an outcome; it neither selects a backend nor persists records.

use std::sync::Arc;

use sylvander_agent::tools::MemoryStore;

use self::session::SessionStore;
use self::session::SqliteSessionStore;

/// Runtime-owned durable component represented in an operational snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeStorageComponent {
    Sessions,
    RelationshipMemory,
}

/// Content-safe availability state of one durable component.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeStorageStatus {
    /// A live integrity check completed successfully.
    Ready,
    /// No concrete production probe is installed, as in isolated unit boot.
    Unverified,
    /// The live integrity check failed; details remain inside Runtime.
    Degraded,
}

/// One redacted storage health observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeStorageHealth {
    pub component: RuntimeStorageComponent,
    pub status: RuntimeStorageStatus,
}

/// Unified, content-free health view over Runtime-owned durable state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeStorageSnapshot {
    pub components: Vec<RuntimeStorageHealth>,
}

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
    session_probe: Option<SqliteSessionStore>,
    memory_probe: Option<memory::SqliteMemoryStore>,
}

impl RuntimeStorage {
    /// Freeze the repositories selected by the Runtime composition root.
    pub(crate) fn new(sessions: Arc<dyn SessionStore>, memory: Arc<dyn MemoryStore>) -> Self {
        Self {
            sessions,
            memory,
            session_probe: None,
            memory_probe: None,
        }
    }

    /// Attach the concrete stores selected during production composition.
    /// Agent-facing trait objects remain unchanged; only Runtime can probe the
    /// backend-specific integrity mechanisms.
    pub(crate) fn with_health_probes(
        mut self,
        sessions: SqliteSessionStore,
        memory: memory::SqliteMemoryStore,
    ) -> Self {
        self.session_probe = Some(sessions);
        self.memory_probe = Some(memory);
        self
    }

    /// Access Session persistence inside Runtime-owned application services.
    pub(crate) fn sessions(&self) -> &Arc<dyn SessionStore> {
        &self.sessions
    }

    /// Probe both authoritative stores concurrently and redact all failures.
    pub(crate) async fn operational_snapshot(&self) -> RuntimeStorageSnapshot {
        let session_probe = self.session_probe.clone();
        let memory_probe = self.memory_probe.clone();
        let session_health = async move {
            match session_probe {
                Some(store) if store.verify_health().await.is_ok() => RuntimeStorageStatus::Ready,
                Some(_) => RuntimeStorageStatus::Degraded,
                None => RuntimeStorageStatus::Unverified,
            }
        };
        let memory_health = tokio::task::spawn_blocking(move || match memory_probe {
            Some(store) if store.verify_health().is_ok() => RuntimeStorageStatus::Ready,
            Some(_) => RuntimeStorageStatus::Degraded,
            None => RuntimeStorageStatus::Unverified,
        });
        let (sessions, memory) = tokio::join!(session_health, memory_health);
        let memory = memory.unwrap_or(RuntimeStorageStatus::Degraded);
        RuntimeStorageSnapshot {
            components: vec![
                RuntimeStorageHealth {
                    component: RuntimeStorageComponent::Sessions,
                    status: sessions,
                },
                RuntimeStorageHealth {
                    component: RuntimeStorageComponent::RelationshipMemory,
                    status: memory,
                },
            ],
        }
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
/// Session metadata, transcript, usage, and authoritative turn lifecycle.
///
/// A successful turn commits its assistant message and terminal state through
/// this module. The separate Evidence recorder is an asynchronous governance
/// projection and must never be used as the Session commit authority.
pub mod session;
/// Filesystem adapter for oversized tool-result artifacts.
#[allow(dead_code)]
// wired when Runtime compression policy gains an explicit artifact root
pub(crate) mod tool_result_disk;
/// Filesystem-backed workspace mutation journal and rollback recovery.
pub(crate) mod workspace_journal;
