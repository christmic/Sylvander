//! Durable storage owned by the Runtime.
//!
//! Storage records lifecycle state and artifacts produced around Agent
//! execution. The Agent kernel receives an immutable conversation snapshot and
//! returns an outcome; it neither selects a backend nor persists records.

use std::sync::Arc;

use sylvander_agent::tools::MemoryStore;

use crate::credential::audit::CredentialOperationAuditLedger;
use crate::evidence::EvidenceStore;
use crate::guardian_runtime::GuardianStorageProbe;
use crate::registry::agent::AgentRegistry;
use crate::user_profile_store::UserProfileStore;

use self::session::SessionStore;
use self::session::SqliteSessionStore;

/// Encrypted, turn-bound artifact retention outside model context.
pub(crate) mod artifact;

/// Runtime-owned durable component represented in an operational snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeStorageComponent {
    Sessions,
    RelationshipMemory,
    AgentRegistry,
    UserProfiles,
    Evidence,
    Artifacts,
    CredentialAudit,
    GuardianCuration,
    GuardianCanonical,
}

/// Content-safe availability state of one durable component.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeStorageStatus {
    /// A live integrity check completed successfully.
    Ready,
    /// No concrete probe is installed, as for an optional disabled component
    /// or isolated unit composition.
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
    agent_registry_probe: Option<AgentRegistry>,
    user_profile_probe: Option<UserProfileStore>,
    evidence_probe: Option<EvidenceStore>,
    artifact_probe: Option<EvidenceStore>,
    credential_audit_probe: Option<Arc<CredentialOperationAuditLedger>>,
    guardian_probe: Option<GuardianStorageProbe>,
}

impl RuntimeStorage {
    /// Freeze the repositories selected by the Runtime composition root.
    pub(crate) fn new(sessions: Arc<dyn SessionStore>, memory: Arc<dyn MemoryStore>) -> Self {
        Self {
            sessions,
            memory,
            session_probe: None,
            memory_probe: None,
            agent_registry_probe: None,
            user_profile_probe: None,
            evidence_probe: None,
            artifact_probe: None,
            credential_audit_probe: None,
            guardian_probe: None,
        }
    }

    /// Attach the concrete stores selected during production composition.
    /// Agent-facing trait objects remain unchanged; only Runtime can probe the
    /// backend-specific integrity mechanisms.
    pub(crate) fn with_health_probes(
        mut self,
        sessions: SqliteSessionStore,
        memory: memory::SqliteMemoryStore,
        agent_registry: AgentRegistry,
        user_profiles: UserProfileStore,
        evidence: EvidenceStore,
        credential_audit: Arc<CredentialOperationAuditLedger>,
    ) -> Self {
        self.session_probe = Some(sessions);
        self.memory_probe = Some(memory);
        self.agent_registry_probe = Some(agent_registry);
        self.user_profile_probe = Some(user_profiles);
        self.artifact_probe = evidence.governance_enabled().then(|| evidence.clone());
        self.evidence_probe = Some(evidence);
        self.credential_audit_probe = Some(credential_audit);
        self
    }

    /// Attach Guardian's health-only authority after its supervisor starts.
    pub(crate) fn with_guardian_probe(mut self, guardian: GuardianStorageProbe) -> Self {
        self.guardian_probe = Some(guardian);
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
        let agent_registry_probe = self.agent_registry_probe.clone();
        let user_profile_probe = self.user_profile_probe.clone();
        let evidence_probe = self.evidence_probe.clone();
        let artifact_probe = self.artifact_probe.clone();
        let credential_audit_probe = self.credential_audit_probe.clone();
        let guardian_curation_probe = self.guardian_probe.clone();
        let guardian_canonical_probe = self.guardian_probe.clone();
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
        let agent_registry_health = async move {
            match agent_registry_probe {
                Some(store) if store.verify_health().await.is_ok() => RuntimeStorageStatus::Ready,
                Some(_) => RuntimeStorageStatus::Degraded,
                None => RuntimeStorageStatus::Unverified,
            }
        };
        let user_profile_health = async move {
            match user_profile_probe {
                Some(store) if store.verify_health().await.is_ok() => RuntimeStorageStatus::Ready,
                Some(_) => RuntimeStorageStatus::Degraded,
                None => RuntimeStorageStatus::Unverified,
            }
        };
        let evidence_health = async move {
            match evidence_probe {
                Some(store) if store.verify_health().await.is_ok() => RuntimeStorageStatus::Ready,
                Some(_) => RuntimeStorageStatus::Degraded,
                None => RuntimeStorageStatus::Unverified,
            }
        };
        let artifact_health = async move {
            match artifact_probe {
                Some(store) if store.verify_health().await.is_ok() => RuntimeStorageStatus::Ready,
                Some(_) => RuntimeStorageStatus::Degraded,
                None => RuntimeStorageStatus::Unverified,
            }
        };
        let credential_audit_health = async move {
            match credential_audit_probe {
                Some(store) if store.verify_health().await.is_ok() => RuntimeStorageStatus::Ready,
                Some(_) => RuntimeStorageStatus::Degraded,
                None => RuntimeStorageStatus::Unverified,
            }
        };
        let guardian_curation_health = async move {
            match guardian_curation_probe {
                Some(store) if store.verify_curation().await.is_ok() => RuntimeStorageStatus::Ready,
                Some(_) => RuntimeStorageStatus::Degraded,
                None => RuntimeStorageStatus::Unverified,
            }
        };
        let guardian_canonical_health = async move {
            match guardian_canonical_probe {
                Some(store) if store.verify_canonical().await.is_ok() => {
                    RuntimeStorageStatus::Ready
                }
                Some(_) => RuntimeStorageStatus::Degraded,
                None => RuntimeStorageStatus::Unverified,
            }
        };
        let (
            sessions,
            memory,
            agent_registry,
            user_profiles,
            evidence,
            artifacts,
            credential_audit,
            guardian_curation,
            guardian_canonical,
        ) = tokio::join!(
            session_health,
            memory_health,
            agent_registry_health,
            user_profile_health,
            evidence_health,
            artifact_health,
            credential_audit_health,
            guardian_curation_health,
            guardian_canonical_health
        );
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
                RuntimeStorageHealth {
                    component: RuntimeStorageComponent::AgentRegistry,
                    status: agent_registry,
                },
                RuntimeStorageHealth {
                    component: RuntimeStorageComponent::UserProfiles,
                    status: user_profiles,
                },
                RuntimeStorageHealth {
                    component: RuntimeStorageComponent::Evidence,
                    status: evidence,
                },
                RuntimeStorageHealth {
                    component: RuntimeStorageComponent::Artifacts,
                    status: artifacts,
                },
                RuntimeStorageHealth {
                    component: RuntimeStorageComponent::CredentialAudit,
                    status: credential_audit,
                },
                RuntimeStorageHealth {
                    component: RuntimeStorageComponent::GuardianCuration,
                    status: guardian_curation,
                },
                RuntimeStorageHealth {
                    component: RuntimeStorageComponent::GuardianCanonical,
                    status: guardian_canonical,
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
/// Filesystem-backed workspace mutation journal and rollback recovery.
pub(crate) mod workspace_journal;
