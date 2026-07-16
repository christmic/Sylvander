//! SQLite-backed relationship memory with versioned, fail-closed migrations.

use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};

use async_trait::async_trait;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::de::DeserializeOwned;
use sylvander_protocol::types::{AgentId, UserId};

use super::memory::{
    Importance, MAX_MEMORY_QUERY_BYTES, MAX_MEMORY_RESULTS, MemoryAppend, MemoryEntry,
    MemoryExecutionContext, MemoryFilter, MemoryOwner, MemoryPatch, MemoryProvenance,
    MemoryProvenanceSource, MemoryStore, MemoryStoreError, RelationshipMemoryRetentionPolicy,
    apply_patch, memory_not_visible, next_revision, validate_append, validate_memory_id,
    validate_patch, validate_revision,
};

mod backup;
mod integrity;
pub use backup::{
    MemoryBackupArtifact, MemoryBackupManifest, MemoryRestoreError, SqliteMemoryAdmin,
};
pub use integrity::{FileMemoryIntegrityAnchor, MemoryIntegrityConfig};

const COMPONENT: &str = "relationship_memory";
const SCHEMA_VERSION: i64 = 6;
const MAX_IDENTIFIER_ATTEMPTS: usize = 8;
const MAX_UNCONFIRMED_CLOCK_FORWARD_SECS: i64 = 31 * 24 * 60 * 60;
const LEDGER_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS memory_schema_migrations (component TEXT PRIMARY KEY, version INTEGER NOT NULL CHECK (version > 0));";
const SCHEMA: &str = r"
CREATE TABLE IF NOT EXISTS relationship_memories (
  record_key TEXT NOT NULL UNIQUE,
  owner_user TEXT NOT NULL,
  owner_agent TEXT NOT NULL,
  id TEXT NOT NULL,
  kind_json TEXT NOT NULL,
  content TEXT NOT NULL,
  references_json TEXT NOT NULL,
  tags_json TEXT NOT NULL,
  importance INTEGER NOT NULL CHECK (importance BETWEEN 0 AND 3),
  created_at INTEGER NOT NULL,
  last_accessed INTEGER,
  access_count INTEGER NOT NULL CHECK (access_count >= 0),
  metadata_json TEXT NOT NULL,
  revision INTEGER NOT NULL CHECK (revision >= 1),
  updated_at INTEGER NOT NULL,
  expires_at INTEGER,
  superseded_by_record_key TEXT,
  origin_actor_kind TEXT NOT NULL,
  origin_user_id TEXT,
  origin_agent_id TEXT,
  origin_session_id TEXT,
  origin_trace_id TEXT,
  origin_source TEXT NOT NULL,
  provenance_trusted INTEGER NOT NULL CHECK (provenance_trusted IN (0, 1)),
  retention_policy_revision INTEGER NOT NULL CHECK (retention_policy_revision >= 1),
  integrity_epoch INTEGER NOT NULL CHECK (integrity_epoch >= 0),
  integrity_mac TEXT NOT NULL CHECK (length(integrity_mac) IN (0, 64)),
  PRIMARY KEY (owner_user, owner_agent, id)
);
CREATE INDEX IF NOT EXISTS relationship_memories_search
  ON relationship_memories(owner_user, owner_agent, importance DESC, created_at DESC, id ASC);
CREATE TABLE IF NOT EXISTS relationship_memory_audit (
  sequence INTEGER PRIMARY KEY AUTOINCREMENT,
  event_id TEXT NOT NULL UNIQUE,
  occurred_at INTEGER NOT NULL,
  operation TEXT NOT NULL,
  target_record_key TEXT NOT NULL,
  before_revision INTEGER,
  after_revision INTEGER,
  actor_kind TEXT NOT NULL,
  actor_user_id TEXT,
  actor_agent_id TEXT,
  session_id TEXT,
  trace_id TEXT,
  changed_mask INTEGER NOT NULL
);
CREATE TRIGGER IF NOT EXISTS relationship_memory_audit_no_update
BEFORE UPDATE ON relationship_memory_audit BEGIN
  SELECT RAISE(ABORT, 'memory audit is append-only');
END;
CREATE TRIGGER IF NOT EXISTS relationship_memory_audit_no_delete
BEFORE DELETE ON relationship_memory_audit BEGIN
  SELECT RAISE(ABORT, 'memory audit is append-only');
END;
CREATE TABLE IF NOT EXISTS relationship_memory_retention_state (
  singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
  clock_watermark INTEGER NOT NULL,
  quarantined_forward_time INTEGER,
  quarantined_observed_at INTEGER,
  last_confirmed_forward_time INTEGER,
  policy_revision INTEGER NOT NULL CHECK (policy_revision >= 1),
  default_ttl_days INTEGER NOT NULL CHECK (default_ttl_days > 0),
  max_ttl_days INTEGER NOT NULL CHECK (max_ttl_days >= default_ttl_days),
  expiry_grace_days INTEGER NOT NULL CHECK (expiry_grace_days >= 0),
  superseded_retention_days INTEGER NOT NULL CHECK (superseded_retention_days >= 0),
  batch_limit INTEGER NOT NULL CHECK (batch_limit > 0),
  CHECK ((quarantined_forward_time IS NULL AND quarantined_observed_at IS NULL)
      OR (quarantined_forward_time > clock_watermark AND quarantined_observed_at IS NOT NULL)),
  CHECK (last_confirmed_forward_time IS NULL OR last_confirmed_forward_time <= clock_watermark)
);
CREATE TABLE IF NOT EXISTS relationship_memory_retention_runs (
  run_id TEXT PRIMARY KEY,
  started_at INTEGER NOT NULL,
  completed_at INTEGER NOT NULL,
  policy_revision INTEGER NOT NULL CHECK (policy_revision >= 1),
  clock_watermark INTEGER NOT NULL,
  expired_count INTEGER NOT NULL CHECK (expired_count >= 0),
  superseded_count INTEGER NOT NULL CHECK (superseded_count >= 0)
);
CREATE TABLE IF NOT EXISTS relationship_memory_retention_batches (
  batch_id TEXT PRIMARY KEY,
  run_id TEXT NOT NULL REFERENCES relationship_memory_retention_runs(run_id),
  occurred_at INTEGER NOT NULL,
  attempted_limit INTEGER NOT NULL CHECK (attempted_limit > 0),
  expired_count INTEGER NOT NULL CHECK (expired_count >= 0),
  superseded_count INTEGER NOT NULL CHECK (superseded_count >= 0)
);
CREATE TABLE IF NOT EXISTS relationship_memory_retention_policy_stage (
  singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
  stage_id TEXT NOT NULL UNIQUE,
  base_policy_revision INTEGER CHECK (base_policy_revision >= 1),
  staged_at INTEGER NOT NULL,
  policy_revision INTEGER NOT NULL CHECK (policy_revision >= 1),
  default_ttl_days INTEGER NOT NULL CHECK (default_ttl_days > 0),
  max_ttl_days INTEGER NOT NULL CHECK (max_ttl_days >= default_ttl_days),
  expiry_grace_days INTEGER NOT NULL CHECK (expiry_grace_days >= 0),
  superseded_retention_days INTEGER NOT NULL CHECK (superseded_retention_days >= 0),
  batch_limit INTEGER NOT NULL CHECK (batch_limit > 0)
);
";
const ENTRY_SELECT: &str = "SELECT m.id, m.kind_json, m.content, m.references_json, m.tags_json, m.importance, m.created_at, m.last_accessed, m.access_count, m.metadata_json, m.revision, m.updated_at, m.expires_at, replacement.id, m.origin_actor_kind, m.origin_user_id, m.origin_agent_id, m.origin_session_id, m.origin_trace_id, m.origin_source, m.provenance_trusted, m.retention_policy_revision, m.record_key, m.owner_user, m.owner_agent, m.id, m.kind_json, m.content, m.references_json, m.tags_json, m.importance, m.created_at, m.last_accessed, m.access_count, m.metadata_json, m.revision, m.updated_at, m.expires_at, m.superseded_by_record_key, m.origin_actor_kind, m.origin_user_id, m.origin_agent_id, m.origin_session_id, m.origin_trace_id, m.origin_source, m.provenance_trusted, m.retention_policy_revision, m.integrity_epoch, m.integrity_mac FROM relationship_memories m LEFT JOIN relationship_memories replacement ON replacement.record_key = m.superseded_by_record_key";

/// Durable implementation of the relationship-only [`MemoryStore`] contract.
#[derive(Clone)]
pub struct SqliteMemoryStore {
    connection: Arc<Mutex<Connection>>,
    active_retention_policy: Arc<RwLock<Option<RelationshipMemoryRetentionPolicy>>>,
    desired_retention_policy: RelationshipMemoryRetentionPolicy,
    staged_activation_id: Arc<Mutex<Option<String>>>,
    clock: Arc<dyn MemoryClock>,
    integrity: Option<Arc<integrity::IntegrityState>>,
}

/// Store-controlled wall clock. Runtime uses [`SystemMemoryClock`]; tests can
/// inject a deterministic clock to exercise rollback and forward-jump safety.
pub trait MemoryClock: Send + Sync {
    fn now_secs(&self) -> i64;
}

#[derive(Debug, Default)]
pub struct SystemMemoryClock;

impl MemoryClock for SystemMemoryClock {
    fn now_secs(&self) -> i64 {
        crate::session::now_secs()
    }
}

/// Store-internal maintenance capability. It is intentionally absent from
/// [`MemoryStore`] and therefore cannot be registered as a model tool.
#[derive(Clone, Debug)]
pub struct SqliteMemoryMaintenance {
    store: SqliteMemoryStore,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryPurgeReport {
    pub expired_count: u32,
    pub superseded_count: u32,
}

/// Result of one maintenance-authorized supersession-chain erasure.
///
/// This report contains counts only. Record content never crosses the
/// maintenance boundary or enters the audit log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryChainForgetReport {
    pub deleted_count: u32,
}

impl MemoryPurgeReport {
    #[must_use]
    pub const fn total_count(self) -> u32 {
        self.expired_count + self.superseded_count
    }
}

impl std::fmt::Debug for SqliteMemoryStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteMemoryStore").finish_non_exhaustive()
    }
}

impl SqliteMemoryStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, MemoryStoreError> {
        Self::open_with_retention_policy(path, RelationshipMemoryRetentionPolicy::default())
    }

    pub fn open_with_retention_policy(
        path: impl AsRef<Path>,
        policy: RelationshipMemoryRetentionPolicy,
    ) -> Result<Self, MemoryStoreError> {
        Self::open_with_retention_policy_and_clock(path, policy, Arc::new(SystemMemoryClock))
    }

    pub fn open_with_retention_policy_and_clock(
        path: impl AsRef<Path>,
        policy: RelationshipMemoryRetentionPolicy,
        clock: Arc<dyn MemoryClock>,
    ) -> Result<Self, MemoryStoreError> {
        Self::from_connection(Connection::open(path).map_err(store_error)?, policy, clock)
    }

    /// Opens a production store protected by an independent, authenticated
    /// integrity anchor. An existing database without a valid anchor is
    /// rejected; only a newly-created database may establish a new anchor.
    pub fn open_with_integrity(
        path: impl AsRef<Path>,
        policy: RelationshipMemoryRetentionPolicy,
        config: MemoryIntegrityConfig,
    ) -> Result<Self, MemoryStoreError> {
        let path = path.as_ref();
        let existed = path.exists();
        let mut connection = Connection::open(path).map_err(store_error)?;
        connection
            .busy_timeout(std::time::Duration::from_secs(5))
            .map_err(store_error)?;
        let integrity = Arc::new(integrity::IntegrityState::new(config));
        let active_policy = if existed {
            verify_schema(&connection)?;
            integrity.verify(&connection)?;
            load_active_policy(&connection)?
        } else {
            migrate(&mut connection)?;
            integrity.establish(&connection)?;
            None
        };
        let store = Self {
            connection: Arc::new(Mutex::new(connection)),
            active_retention_policy: Arc::new(RwLock::new(active_policy)),
            desired_retention_policy: policy,
            staged_activation_id: Arc::new(Mutex::new(None)),
            clock: Arc::new(SystemMemoryClock),
            integrity: Some(integrity),
        };
        let now = store.clock.now_secs();
        let policy = store.desired_retention_policy.clone();
        let stage_id = store
            .with_connection(|transaction| stage_policy_transaction(transaction, &policy, now))?;
        *store
            .staged_activation_id
            .lock()
            .map_err(|_| store_failure())? = stage_id;
        Ok(store)
    }

    pub fn open_in_memory() -> Result<Self, MemoryStoreError> {
        Self::open_in_memory_with_retention_policy(RelationshipMemoryRetentionPolicy::default())
    }

    pub fn open_in_memory_with_retention_policy(
        policy: RelationshipMemoryRetentionPolicy,
    ) -> Result<Self, MemoryStoreError> {
        Self::open_in_memory_with_retention_policy_and_clock(policy, Arc::new(SystemMemoryClock))
    }

    pub fn open_in_memory_with_retention_policy_and_clock(
        policy: RelationshipMemoryRetentionPolicy,
        clock: Arc<dyn MemoryClock>,
    ) -> Result<Self, MemoryStoreError> {
        Self::from_connection(
            Connection::open_in_memory().map_err(store_error)?,
            policy,
            clock,
        )
    }

    fn from_connection(
        mut connection: Connection,
        retention_policy: RelationshipMemoryRetentionPolicy,
        clock: Arc<dyn MemoryClock>,
    ) -> Result<Self, MemoryStoreError> {
        connection
            .busy_timeout(std::time::Duration::from_secs(5))
            .map_err(store_error)?;
        migrate(&mut connection)?;
        activate_policy(&mut connection, &retention_policy, clock.now_secs())?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            active_retention_policy: Arc::new(RwLock::new(Some(retention_policy.clone()))),
            desired_retention_policy: retention_policy,
            staged_activation_id: Arc::new(Mutex::new(None)),
            clock,
            integrity: None,
        })
    }

    fn with_connection<T>(
        &self,
        operation: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<T, MemoryStoreError>,
    ) -> Result<T, MemoryStoreError> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| MemoryStoreError::Store("memory database lock poisoned".into()))?;
        // Serialize independent store instances in SQLite before reading the
        // external anchor. Otherwise one writer could advance the database
        // and anchor between another writer's verification and transaction.
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(store_error)?;
        let before = self
            .integrity
            .as_ref()
            .map(|integrity| integrity.verify(&transaction))
            .transpose()?;
        let current_epoch = self
            .integrity
            .as_ref()
            .map(|integrity| integrity.snapshot().map(|(epoch, _)| epoch))
            .transpose()?;
        let result = operation(&transaction)?;
        if let (Some(integrity), Some(before), Some(current_epoch)) =
            (&self.integrity, before, current_epoch)
        {
            let to_epoch = current_epoch.checked_add(1).ok_or_else(store_failure)?;
            integrity.seal_rows(&transaction, to_epoch)?;
            let after = integrity::database_root(&transaction)?;
            if before != after {
                integrity.prepare(&before, &after)?;
                transaction.commit().map_err(store_error)?;
                integrity.finalize(&after)?;
                return Ok(result);
            }
        }
        transaction.commit().map_err(store_error)?;
        Ok(result)
    }

    fn with_raw_connection<T>(
        &self,
        operation: impl FnOnce(&mut Connection) -> Result<T, MemoryStoreError>,
    ) -> Result<T, MemoryStoreError> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| MemoryStoreError::Store("memory database lock poisoned".into()))?;
        if let Some(integrity) = &self.integrity {
            integrity.verify(&connection)?;
        }
        operation(&mut connection)
    }

    fn with_read_connection<T>(
        &self,
        operation: impl FnOnce(&mut Connection) -> Result<T, MemoryStoreError>,
    ) -> Result<T, MemoryStoreError> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| MemoryStoreError::Store("memory database lock poisoned".into()))?;
        operation(&mut connection)
    }

    #[must_use]
    pub fn maintenance(&self) -> SqliteMemoryMaintenance {
        SqliteMemoryMaintenance {
            store: self.clone(),
        }
    }

    fn retention_policy(&self) -> Result<RelationshipMemoryRetentionPolicy, MemoryStoreError> {
        self.active_retention_policy
            .read()
            .map_err(|_| store_failure())?
            .clone()
            .ok_or_else(retention_error)
    }
}

impl SqliteMemoryMaintenance {
    /// Returns whether this database already has an active retention policy.
    /// A brand-new protected store remains unavailable until Runtime commits
    /// its staged policy after every other startup readiness check succeeds.
    pub fn has_active_retention_policy(&self) -> Result<bool, MemoryStoreError> {
        Ok(self
            .store
            .active_retention_policy
            .read()
            .map_err(|_| store_failure())?
            .is_some())
    }

    /// Atomically activates the policy staged when the protected store opened.
    ///
    /// The opaque stage identifier is compared in `SQLite`, so another startup
    /// may safely replace a stale proposal without letting this process commit
    /// it. Repeating activation after the same policy won the race is
    /// idempotent.
    pub fn activate_staged_retention_policy(&self) -> Result<(), MemoryStoreError> {
        let stage_id = self
            .store
            .staged_activation_id
            .lock()
            .map_err(|_| store_failure())?
            .clone();
        let desired = self.store.desired_retention_policy.clone();
        let now = self.store.clock.now_secs();
        self.store.with_connection(|transaction| {
            activate_staged_policy_transaction(transaction, &desired, stage_id.as_deref(), now)
        })?;
        *self
            .store
            .active_retention_policy
            .write()
            .map_err(|_| store_failure())? = Some(desired);
        Ok(())
    }

    pub fn purge(&self) -> Result<MemoryPurgeReport, MemoryStoreError> {
        self.purge_at(self.store.clock.now_secs())
    }

    /// Confirms the quarantined wall-clock value after an operator has
    /// verified that the forward jump is real. Model-facing tools cannot
    /// obtain this maintenance capability.
    pub fn confirm_quarantined_clock(&self) -> Result<i64, MemoryStoreError> {
        self.store.with_connection(|transaction| {
            let candidate: i64 = transaction
                .query_row(
                    "SELECT quarantined_forward_time FROM relationship_memory_retention_state WHERE singleton = 1 AND quarantined_forward_time IS NOT NULL",
                    [],
                    |row| row.get(0),
                )
                .map_err(|_| retention_error())?;
            transaction
                .execute(
                    "UPDATE relationship_memory_retention_state SET clock_watermark = ?1, quarantined_forward_time = NULL, quarantined_observed_at = NULL, last_confirmed_forward_time = ?1 WHERE singleton = 1",
                    [candidate],
                )
                .map_err(|_| retention_error())?;
            Ok(candidate)
        })
    }

    pub fn backup_to_data_dir(
        &self,
        data_dir: impl AsRef<Path>,
    ) -> Result<MemoryBackupArtifact, MemoryStoreError> {
        backup::create_backup(&self.store, data_dir.as_ref())
    }

    /// Publish one verified backup and then retain only the newest verified
    /// database/manifest pairs. Incomplete, temporary, or invalid artifacts
    /// never count toward the retention limit.
    pub fn backup_and_rotate(
        &self,
        data_dir: impl AsRef<Path>,
        retained_copies: u32,
    ) -> Result<MemoryBackupArtifact, MemoryStoreError> {
        backup::create_backup_and_rotate(&self.store, data_dir.as_ref(), retained_copies)
    }

    /// Atomically erases an active relationship memory and every superseded
    /// ancestor that leads to it.
    ///
    /// This operation deliberately lives on the store-issued maintenance
    /// capability rather than [`MemoryStore`], so a model-facing tool cannot
    /// mint management authority. Ordinary single-record deletion remains
    /// restricted while an inbound supersession link exists.
    pub fn forget_supersession_chain(
        &self,
        owner: &MemoryOwner,
        id: &str,
        expected_revision: u64,
    ) -> Result<MemoryChainForgetReport, MemoryStoreError> {
        let MemoryOwner::Relationship { user_id, agent_id } = owner else {
            return Err(MemoryStoreError::InvalidInput);
        };
        validate_memory_id(id)?;
        validate_revision(expected_revision)?;
        let expected_revision =
            i64::try_from(expected_revision).map_err(|_| MemoryStoreError::InvalidInput)?;
        let wall_now = self.store.clock.now_secs();
        let policy = self.store.retention_policy()?;
        self.store.with_connection(|transaction| {
            let now = resolve_effective_now(
                transaction,
                policy.revision(),
                wall_now,
            )
            .map_err(|_| forget_error())?
            .now;
            let current: Option<(String, i64)> = transaction
                .query_row(
                    "SELECT record_key, revision FROM relationship_memories WHERE owner_user = ?1 AND owner_agent = ?2 AND id = ?3 AND superseded_by_record_key IS NULL AND (expires_at IS NULL OR expires_at > ?4)",
                    params![user_id.0, agent_id.0, id, now],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(|_| forget_error())?;
            let Some((current_key, current_revision)) = current else {
                return Err(memory_not_visible());
            };
            if current_revision != expected_revision {
                return Err(MemoryStoreError::Conflict);
            }

            let chain = {
                let mut statement = transaction
                    .prepare(
                        "WITH RECURSIVE chain(record_key, revision) AS (SELECT record_key, revision FROM relationship_memories WHERE record_key = ?1 UNION SELECT ancestor.record_key, ancestor.revision FROM relationship_memories ancestor JOIN chain descendant ON ancestor.superseded_by_record_key = descendant.record_key WHERE ancestor.owner_user = ?2 AND ancestor.owner_agent = ?3) SELECT record_key, revision FROM chain ORDER BY record_key",
                    )
                    .map_err(|_| forget_error())?;
                statement
                    .query_map(params![current_key, user_id.0, agent_id.0], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                    })
                    .map_err(|_| forget_error())?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|_| forget_error())?
            };
            let deleted_count =
                u32::try_from(chain.len()).map_err(|_| MemoryStoreError::InvalidInput)?;
            if deleted_count == 0 {
                return Err(forget_error());
            }

            // A cross-owner or otherwise out-of-closure dependent would be
            // left dangling. Treat externally damaged graph state as a
            // conflict and leave every row untouched.
            let has_external_dependent = transaction
                .query_row(
                    "WITH RECURSIVE chain(record_key) AS (SELECT record_key FROM relationship_memories WHERE record_key = ?1 UNION SELECT ancestor.record_key FROM relationship_memories ancestor JOIN chain descendant ON ancestor.superseded_by_record_key = descendant.record_key WHERE ancestor.owner_user = ?2 AND ancestor.owner_agent = ?3) SELECT EXISTS(SELECT 1 FROM relationship_memories dependent WHERE dependent.superseded_by_record_key IN (SELECT record_key FROM chain) AND dependent.record_key NOT IN (SELECT record_key FROM chain))",
                    params![current_key, user_id.0, agent_id.0],
                    |row| row.get::<_, bool>(0),
                )
                .map_err(|_| forget_error())?;
            if has_external_dependent {
                return Err(MemoryStoreError::Conflict);
            }

            for (record_key, revision) in &chain {
                append_chain_forget_audit(transaction, record_key, *revision, now)?;
            }
            for (record_key, revision) in &chain {
                if transaction
                    .execute(
                        "DELETE FROM relationship_memories WHERE record_key = ?1 AND revision = ?2",
                        params![record_key, revision],
                    )
                    .map_err(|_| forget_error())?
                    != 1
                {
                    return Err(MemoryStoreError::Conflict);
                }
            }
            Ok(MemoryChainForgetReport { deleted_count })
        })
    }

    fn purge_at(&self, wall_now: i64) -> Result<MemoryPurgeReport, MemoryStoreError> {
        let policy = self.store.retention_policy()?;
        let result = self.store.with_connection(|transaction| {
            let clock = resolve_effective_now(transaction, policy.revision(), wall_now)?;
            if clock.quarantined {
                return Ok(None);
            }
            let now = clock.now;
            let grace = i64::from(policy.expiry_grace_days()) * 24 * 60 * 60;
            let superseded_age =
                i64::from(policy.superseded_retention_days()) * 24 * 60 * 60;
            let expired_cutoff = now.checked_sub(grace).ok_or_else(retention_error)?;
            let superseded_cutoff = now
                .checked_sub(superseded_age)
                .ok_or_else(retention_error)?;
            let candidates = {
                let mut statement = transaction.prepare(
                    "SELECT m.record_key, m.revision, CASE WHEN m.superseded_by_record_key IS NOT NULL THEN 1 ELSE 0 END FROM relationship_memories m WHERE ((m.superseded_by_record_key IS NOT NULL AND m.updated_at <= ?1) OR (m.superseded_by_record_key IS NULL AND m.expires_at IS NOT NULL AND m.expires_at <= ?2)) AND NOT EXISTS (SELECT 1 FROM relationship_memories dependent WHERE dependent.superseded_by_record_key = m.record_key) ORDER BY COALESCE(m.expires_at, m.updated_at), m.record_key LIMIT ?3",
                ).map_err(|_| retention_error())?;
                statement
                    .query_map(
                        params![superseded_cutoff, expired_cutoff, policy.batch_limit()],
                        |row| Ok((row.get::<_, String>(0)?, read_revision(row, 1)?, row.get::<_, i64>(2)? == 1)),
                    )
                    .map_err(|_| retention_error())?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|_| retention_error())?
            };
            let mut report = MemoryPurgeReport { expired_count: 0, superseded_count: 0 };
            for (record_key, revision, superseded) in candidates {
                append_maintenance_audit(transaction, &record_key, revision, now, superseded)?;
                if transaction.execute(
                    "DELETE FROM relationship_memories WHERE record_key = ?1 AND revision = ?2 AND NOT EXISTS (SELECT 1 FROM relationship_memories dependent WHERE dependent.superseded_by_record_key = relationship_memories.record_key)",
                    params![record_key, i64::try_from(revision).map_err(|_| retention_error())?],
                ).map_err(|_| retention_error())? != 1 {
                    return Err(retention_error());
                }
                if superseded {
                    report.superseded_count += 1;
                } else {
                    report.expired_count += 1;
                }
            }
            insert_retention_ledgers(transaction, &policy, now, report)?;
            Ok(Some(report))
        })?;
        result.ok_or_else(retention_error)
    }
}

#[async_trait]
impl MemoryStore for SqliteMemoryStore {
    async fn append_relationship(
        &self,
        ctx: &MemoryExecutionContext,
        append: MemoryAppend,
    ) -> Result<MemoryEntry, MemoryStoreError> {
        let owner = ctx.relationship_owner()?;
        let policy = self.retention_policy()?;
        let append = policy.apply_append(append)?;
        validate_append(&append)?;
        let wall_now = self.clock.now_secs();
        let provenance = ctx.provenance();
        self.with_connection(move |transaction| {
            let now = resolve_effective_now(transaction, policy.revision(), wall_now)
                .map_err(|_| store_failure())?
                .now;
            let MemoryOwner::Relationship { user_id, agent_id } = &owner else {
                unreachable!("relationship constructor returned another scope")
            };
            let id = allocate_identifier(
                transaction,
                IdentifierNamespace::Memory { user_id, agent_id },
            )?;
            let record_key = allocate_identifier(transaction, IdentifierNamespace::RecordKey)?;
            let entry = MemoryEntry::materialize(
                id,
                owner,
                append,
                provenance,
                policy.revision(),
                now,
            )?;
            insert_entry(transaction, &record_key, &entry)?;
            append_audit(
                transaction,
                ctx,
                &record_key,
                "append",
                None,
                Some(1),
                now,
                0x3f,
            )?;
            Ok(entry)
        })
    }

    async fn search_relationship(
        &self,
        ctx: &MemoryExecutionContext,
        query: &str,
        filter: MemoryFilter,
    ) -> Result<Vec<MemoryEntry>, MemoryStoreError> {
        let owner = ctx.relationship_owner()?;
        if query.len() > MAX_MEMORY_QUERY_BYTES
            || filter
                .limit
                .is_some_and(|limit| limit == 0 || limit > MAX_MEMORY_RESULTS)
        {
            return Err(MemoryStoreError::InvalidInput);
        }
        let MemoryOwner::Relationship { user_id, agent_id } = owner else {
            unreachable!("relationship constructor returned another scope")
        };
        let query = query.to_lowercase();
        let wall_now = self.clock.now_secs();
        let policy = self.retention_policy()?;
        self.with_read_connection(|connection| {
            let integrity_epoch = self
                .integrity
                .as_ref()
                .map(|integrity| integrity.read_epoch(connection))
                .transpose()?;
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(search_error)?;
            let now = resolve_effective_now(
                &transaction,
                policy.revision(),
                wall_now,
            )
            .map_err(|_| search_error(rusqlite::Error::InvalidQuery))?
            .now;
            let mut statement = transaction
                .prepare(&format!("{ENTRY_SELECT} WHERE m.owner_user = ?1 AND m.owner_agent = ?2 AND m.superseded_by_record_key IS NULL AND (m.expires_at IS NULL OR m.expires_at > ?3) ORDER BY m.importance DESC, m.created_at DESC, m.id ASC"))
                .map_err(search_error)?;
            let rows = statement
                .query_map(params![user_id.0, agent_id.0, now], |row| {
                    if let (Some(integrity), Some(epoch)) = (&self.integrity, integrity_epoch) {
                        integrity.verify_row(row, 22, epoch)?;
                    }
                    decode_row(row, &user_id, &agent_id)
                })
                .map_err(search_error)?;
            let mut results = Vec::new();
            for row in rows {
                let entry = row.map_err(search_error)?;
                if (!query.is_empty() && !entry.content.to_lowercase().contains(&query))
                    || filter.kind.as_ref().is_some_and(|kind| kind != &entry.kind)
                    || filter
                        .min_importance
                        .is_some_and(|importance| entry.importance < importance)
                {
                    continue;
                }
                results.push(entry);
                if filter.limit == Some(results.len()) {
                    break;
                }
            }
            drop(statement);
            transaction.commit().map_err(search_error)?;
            Ok(results)
        })
    }

    async fn update_relationship(
        &self,
        ctx: &MemoryExecutionContext,
        id: &str,
        expected_revision: u64,
        patch: MemoryPatch,
    ) -> Result<MemoryEntry, MemoryStoreError> {
        let MemoryOwner::Relationship { user_id, agent_id } = ctx.relationship_owner()? else {
            unreachable!("relationship constructor returned another scope")
        };
        validate_memory_id(id)?;
        validate_patch(&patch)?;
        let policy = self.retention_policy()?;
        policy.validate_patch(&patch)?;
        let updates_expiry = patch.expiry.is_some();
        validate_revision(expected_revision)?;
        let wall_now = self.clock.now_secs();
        self.with_connection(|transaction| {
            let now = resolve_effective_now(
                transaction,
                policy.revision(),
                wall_now,
            )
            .map_err(|_| mutation_error())?
            .now;
            let (record_key, revision) = select_active_record(
                transaction,
                &user_id,
                &agent_id,
                id,
                now,
            )?
            .ok_or_else(memory_not_visible)?;
            if revision != expected_revision {
                return Err(MemoryStoreError::Conflict);
            }
            let mut entry = transaction
                .query_row(
                    &format!("{ENTRY_SELECT} WHERE m.record_key = ?1"),
                    [&record_key],
                    |row| decode_row(row, &user_id, &agent_id),
                )
                .map_err(|_| mutation_error())?;
            apply_patch(&mut entry, patch, now)?;
            if updates_expiry {
                entry.retention_policy_revision = policy.revision();
            }
            let changed = transaction
                .execute(
                    "UPDATE relationship_memories SET kind_json = ?1, content = ?2, references_json = ?3, tags_json = ?4, importance = ?5, metadata_json = ?6, revision = ?7, updated_at = ?8, expires_at = ?9, retention_policy_revision = ?10 WHERE record_key = ?11 AND revision = ?12 AND superseded_by_record_key IS NULL AND (expires_at IS NULL OR expires_at > ?8)",
                    params![encode(&entry.kind).map_err(|_| mutation_error())?, entry.content, encode(&entry.references).map_err(|_| mutation_error())?, encode(&entry.tags).map_err(|_| mutation_error())?, importance_value(entry.importance), encode(&entry.metadata).map_err(|_| mutation_error())?, i64::try_from(entry.revision).map_err(|_| MemoryStoreError::InvalidInput)?, entry.updated_at, entry.expires_at, i64::try_from(entry.retention_policy_revision).map_err(|_| MemoryStoreError::InvalidInput)?, record_key, i64::try_from(expected_revision).map_err(|_| MemoryStoreError::InvalidInput)?],
                )
                .map_err(|_| mutation_error())?;
            if changed != 1 {
                return Err(MemoryStoreError::Conflict);
            }
            append_audit(transaction, ctx, &record_key, "update", Some(revision), Some(entry.revision), now, 0x3f).map_err(|_| mutation_error())?;
            Ok(entry)
        })
    }

    async fn supersede_relationship(
        &self,
        ctx: &MemoryExecutionContext,
        id: &str,
        expected_revision: u64,
        replacement: MemoryAppend,
    ) -> Result<MemoryEntry, MemoryStoreError> {
        let owner = ctx.relationship_owner()?;
        validate_memory_id(id)?;
        let policy = self.retention_policy()?;
        let replacement = policy.apply_append(replacement)?;
        validate_append(&replacement)?;
        validate_revision(expected_revision)?;
        let (user_id, agent_id) = match &owner {
            MemoryOwner::Relationship { user_id, agent_id } => (user_id.clone(), agent_id.clone()),
            _ => unreachable!("relationship constructor returned another scope"),
        };
        let wall_now = self.clock.now_secs();
        let provenance = ctx.provenance();
        self.with_connection(move |transaction| {
            let now = resolve_effective_now(transaction, policy.revision(), wall_now)
                .map_err(|_| mutation_error())?
                .now;
            let replacement_id = allocate_identifier(
                transaction,
                IdentifierNamespace::Memory {
                    user_id: &user_id,
                    agent_id: &agent_id,
                },
            )?;
            let replacement_key =
                allocate_identifier(transaction, IdentifierNamespace::RecordKey)?;
            let replacement = MemoryEntry::materialize(
                replacement_id,
                owner,
                replacement,
                provenance,
                policy.revision(),
                now,
            )?;
            let (record_key, revision) = select_active_record(transaction, &user_id, &agent_id, id, now)?.ok_or_else(memory_not_visible)?;
            if revision != expected_revision {
                return Err(MemoryStoreError::Conflict);
            }
            let next = next_revision(revision)?;
            insert_entry(transaction, &replacement_key, &replacement).map_err(|_| mutation_error())?;
            let changed = transaction.execute(
                "UPDATE relationship_memories SET superseded_by_record_key = ?1, revision = ?2, updated_at = ?3 WHERE record_key = ?4 AND revision = ?5 AND superseded_by_record_key IS NULL AND (expires_at IS NULL OR expires_at > ?3) AND record_key <> ?1 AND EXISTS (SELECT 1 FROM relationship_memories replacement WHERE replacement.record_key = ?1 AND replacement.owner_user = relationship_memories.owner_user AND replacement.owner_agent = relationship_memories.owner_agent AND replacement.superseded_by_record_key IS NULL)",
                params![replacement_key, i64::try_from(next).map_err(|_| MemoryStoreError::InvalidInput)?, now, record_key, i64::try_from(expected_revision).map_err(|_| MemoryStoreError::InvalidInput)?],
            ).map_err(|_| mutation_error())?;
            if changed != 1 {
                return Err(MemoryStoreError::Conflict);
            }
            append_audit(transaction, ctx, &replacement_key, "append", None, Some(1), now, 0x3f).map_err(|_| mutation_error())?;
            append_audit(transaction, ctx, &record_key, "supersede", Some(revision), Some(next), now, 0x40).map_err(|_| mutation_error())?;
            Ok(replacement)
        })
    }

    async fn delete_relationship(
        &self,
        ctx: &MemoryExecutionContext,
        id: &str,
        expected_revision: u64,
    ) -> Result<(), MemoryStoreError> {
        let MemoryOwner::Relationship { user_id, agent_id } = ctx.relationship_owner()? else {
            unreachable!("relationship constructor returned another scope")
        };
        validate_memory_id(id)?;
        validate_revision(expected_revision)?;
        let expected_revision =
            i64::try_from(expected_revision).map_err(|_| MemoryStoreError::InvalidInput)?;
        let wall_now = self.clock.now_secs();
        let policy = self.retention_policy()?;
        self.with_connection(|transaction| {
            let now = resolve_effective_now(
                transaction,
                policy.revision(),
                wall_now,
            )
            .map_err(|_| MemoryStoreError::Delete("memory delete failed".into()))?
            .now;
            let visible: Option<(String, i64)> = transaction
                .query_row(
                    "SELECT record_key, revision FROM relationship_memories WHERE owner_user = ?1 AND owner_agent = ?2 AND id = ?3 AND superseded_by_record_key IS NULL AND (expires_at IS NULL OR expires_at > ?4)",
                    params![user_id.0, agent_id.0, id, now],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(delete_error)?;
            let Some((record_key, revision)) = visible else {
                return Err(memory_not_visible());
            };
            if revision != expected_revision {
                return Err(MemoryStoreError::Conflict);
            }
            let changed = transaction
                .execute(
                    "DELETE FROM relationship_memories WHERE record_key = ?1 AND revision = ?2 AND NOT EXISTS (SELECT 1 FROM relationship_memories dependent WHERE dependent.superseded_by_record_key = relationship_memories.record_key)",
                    params![record_key, expected_revision],
                )
                .map_err(delete_error)?;
            if changed != 1 {
                return Err(MemoryStoreError::Conflict);
            }
            append_audit(
                transaction,
                ctx,
                &record_key,
                "delete",
                u64::try_from(revision).ok(),
                None,
                now,
                0,
            )?;
            Ok(())
        })
    }

    async fn get_relationship(
        &self,
        ctx: &MemoryExecutionContext,
        id: &str,
    ) -> Result<Option<MemoryEntry>, MemoryStoreError> {
        let MemoryOwner::Relationship { user_id, agent_id } = ctx.relationship_owner()? else {
            unreachable!("relationship constructor returned another scope")
        };
        validate_memory_id(id)?;
        let wall_now = self.clock.now_secs();
        let policy = self.retention_policy()?;
        self.with_read_connection(|connection| {
            let integrity_epoch = self
                .integrity
                .as_ref()
                .map(|integrity| integrity.read_epoch(connection))
                .transpose()?;
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(store_error)?;
            let now = resolve_effective_now(
                &transaction,
                policy.revision(),
                wall_now,
            )
            .map_err(|_| store_failure())?
            .now;
            let entry = transaction
                .query_row(
                    &format!("{ENTRY_SELECT} WHERE m.owner_user = ?1 AND m.owner_agent = ?2 AND m.id = ?3 AND m.superseded_by_record_key IS NULL AND (m.expires_at IS NULL OR m.expires_at > ?4)"),
                    params![user_id.0, agent_id.0, id, now],
                    |row| {
                        if let (Some(integrity), Some(epoch)) =
                            (&self.integrity, integrity_epoch)
                        {
                            integrity.verify_row(row, 22, epoch)?;
                        }
                        decode_row(row, &user_id, &agent_id)
                    },
                )
                .optional()
                .map_err(store_error)?;
            transaction.commit().map_err(store_error)?;
            Ok(entry)
        })
    }
}

fn migrate(connection: &mut Connection) -> Result<(), MemoryStoreError> {
    let has_component_objects = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name IN ('relationship_memories', 'relationship_memories_search', 'relationship_memory_audit', 'relationship_memory_audit_no_update', 'relationship_memory_audit_no_delete'))",
            [],
            |row| row.get::<_, bool>(0),
        )
        .map_err(|_| schema_error())?;
    let has_ledger = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'memory_schema_migrations')",
            [],
            |row| row.get::<_, bool>(0),
        )
        .map_err(|_| schema_error())?;
    if has_component_objects && !has_ledger {
        return Err(schema_error());
    }
    connection
        .execute_batch(&format!("PRAGMA foreign_keys = ON; {LEDGER_SCHEMA}"))
        .map_err(|_| schema_error())?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(store_error)?;
    let version: Option<i64> = transaction
        .query_row(
            "SELECT version FROM memory_schema_migrations WHERE component = ?1",
            [COMPONENT],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| schema_error())?;
    match version {
        None => {
            if component_objects_exist(&transaction)? {
                return Err(schema_error());
            }
            transaction.execute_batch(SCHEMA).map_err(store_error)?;
            transaction
                .execute(
                    "INSERT INTO memory_schema_migrations(component, version) VALUES (?1, ?2)",
                    params![COMPONENT, SCHEMA_VERSION],
                )
                .map_err(store_error)?;
            verify_schema(&transaction)?;
            transaction.commit().map_err(store_error)
        }
        Some(SCHEMA_VERSION) => {
            verify_schema(&transaction)?;
            transaction.commit().map_err(store_error)
        }
        Some(_) => Err(schema_error()),
    }
}

type SchemaObject = (String, String, String, String);

fn verify_schema(connection: &Connection) -> Result<(), MemoryStoreError> {
    let expected = Connection::open_in_memory().map_err(|_| schema_error())?;
    expected
        .execute_batch(&format!("{LEDGER_SCHEMA}{SCHEMA}"))
        .map_err(|_| schema_error())?;
    if schema_objects(connection)? != schema_objects(&expected)? {
        return Err(schema_error());
    }
    Ok(())
}

fn schema_objects(connection: &Connection) -> Result<Vec<SchemaObject>, MemoryStoreError> {
    let mut statement = connection
        .prepare(
            "SELECT type, name, tbl_name, sql FROM sqlite_master WHERE sql IS NOT NULL AND (name = 'memory_schema_migrations' OR tbl_name IN ('relationship_memories', 'relationship_memory_audit', 'relationship_memory_retention_state', 'relationship_memory_retention_runs', 'relationship_memory_retention_batches', 'relationship_memory_retention_policy_stage')) ORDER BY type, name, tbl_name",
        )
        .map_err(|_| schema_error())?;
    statement
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                normalize_sql(&row.get::<_, String>(3)?),
            ))
        })
        .map_err(|_| schema_error())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| schema_error())
}

fn normalize_sql(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn activate_policy(
    connection: &mut Connection,
    policy: &RelationshipMemoryRetentionPolicy,
    wall_now: i64,
) -> Result<(), MemoryStoreError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| retention_error())?;
    activate_policy_transaction(&transaction, policy, wall_now)?;
    transaction.commit().map_err(|_| retention_error())
}

fn activate_policy_transaction(
    transaction: &rusqlite::Transaction<'_>,
    policy: &RelationshipMemoryRetentionPolicy,
    wall_now: i64,
) -> Result<(), MemoryStoreError> {
    let policy_revision_sql = i64::try_from(policy.revision()).map_err(|_| retention_error())?;
    let existing: Option<(u64, u32, u32, u32, u32, u32, i64)> = transaction
        .query_row(
            "SELECT policy_revision, default_ttl_days, max_ttl_days, expiry_grace_days, superseded_retention_days, batch_limit, clock_watermark FROM relationship_memory_retention_state WHERE singleton = 1",
            [],
            |row| Ok((read_revision(row, 0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)),
        )
        .optional()
        .map_err(|_| retention_error())?;
    if let Some((revision, default, max, grace, superseded, batch, _watermark)) = existing {
        let same = (default, max, grace, superseded, batch)
            == (
                policy.default_ttl_days(),
                policy.max_ttl_days(),
                policy.expiry_grace_days(),
                policy.superseded_retention_days(),
                policy.batch_limit(),
            );
        if revision > policy.revision() || (revision == policy.revision() && !same) {
            return Err(retention_error());
        }
        transaction.execute(
            "UPDATE relationship_memory_retention_state SET policy_revision = ?1, default_ttl_days = ?2, max_ttl_days = ?3, expiry_grace_days = ?4, superseded_retention_days = ?5, batch_limit = ?6 WHERE singleton = 1",
            params![policy_revision_sql, policy.default_ttl_days(), policy.max_ttl_days(), policy.expiry_grace_days(), policy.superseded_retention_days(), policy.batch_limit()],
        ).map_err(|_| retention_error())?;
        resolve_effective_now(transaction, policy.revision(), wall_now)?;
    } else {
        transaction.execute(
            "INSERT INTO relationship_memory_retention_state (singleton, clock_watermark, quarantined_forward_time, quarantined_observed_at, last_confirmed_forward_time, policy_revision, default_ttl_days, max_ttl_days, expiry_grace_days, superseded_retention_days, batch_limit) VALUES (1, ?1, NULL, NULL, NULL, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![wall_now, policy_revision_sql, policy.default_ttl_days(), policy.max_ttl_days(), policy.expiry_grace_days(), policy.superseded_retention_days(), policy.batch_limit()],
        ).map_err(|_| retention_error())?;
    }
    Ok(())
}

fn load_active_policy(
    connection: &Connection,
) -> Result<Option<RelationshipMemoryRetentionPolicy>, MemoryStoreError> {
    connection
        .query_row(
            "SELECT policy_revision, default_ttl_days, max_ttl_days, expiry_grace_days, superseded_retention_days, batch_limit FROM relationship_memory_retention_state WHERE singleton = 1",
            [],
            |row| {
                RelationshipMemoryRetentionPolicy::new(
                    read_revision(row, 0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                )
                .map_err(|_| rusqlite::Error::InvalidQuery)
            },
        )
        .optional()
        .map_err(|_| retention_error())
}

fn stage_policy_transaction(
    transaction: &rusqlite::Transaction<'_>,
    desired: &RelationshipMemoryRetentionPolicy,
    now: i64,
) -> Result<Option<String>, MemoryStoreError> {
    let active = load_active_policy(transaction)?;
    if let Some(active) = &active {
        if active.revision() > desired.revision()
            || (active.revision() == desired.revision() && active != desired)
        {
            return Err(retention_error());
        }
        if active == desired {
            transaction
                .execute(
                    "DELETE FROM relationship_memory_retention_policy_stage WHERE singleton = 1",
                    [],
                )
                .map_err(|_| retention_error())?;
            return Ok(None);
        }
    }

    let base_revision = active
        .as_ref()
        .map(RelationshipMemoryRetentionPolicy::revision);
    let existing: Option<(String, Option<u64>, RelationshipMemoryRetentionPolicy)> = transaction
        .query_row(
            "SELECT stage_id, base_policy_revision, policy_revision, default_ttl_days, max_ttl_days, expiry_grace_days, superseded_retention_days, batch_limit FROM relationship_memory_retention_policy_stage WHERE singleton = 1",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get::<_, Option<i64>>(1)?
                        .map(u64::try_from)
                        .transpose()
                        .map_err(|_| rusqlite::Error::InvalidQuery)?,
                    RelationshipMemoryRetentionPolicy::new(
                        read_revision(row, 2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                    )
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
                ))
            },
        )
        .optional()
        .map_err(|_| retention_error())?;
    if let Some((stage_id, existing_base, existing_policy)) = existing
        && existing_base == base_revision
        && existing_policy == *desired
    {
        return Ok(Some(stage_id));
    }

    let stage_id = uuid::Uuid::new_v4().to_string();
    let base_revision_sql = base_revision
        .map(i64::try_from)
        .transpose()
        .map_err(|_| retention_error())?;
    let revision_sql = i64::try_from(desired.revision()).map_err(|_| retention_error())?;
    transaction
        .execute(
            "INSERT INTO relationship_memory_retention_policy_stage (singleton, stage_id, base_policy_revision, staged_at, policy_revision, default_ttl_days, max_ttl_days, expiry_grace_days, superseded_retention_days, batch_limit) VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) ON CONFLICT(singleton) DO UPDATE SET stage_id = excluded.stage_id, base_policy_revision = excluded.base_policy_revision, staged_at = excluded.staged_at, policy_revision = excluded.policy_revision, default_ttl_days = excluded.default_ttl_days, max_ttl_days = excluded.max_ttl_days, expiry_grace_days = excluded.expiry_grace_days, superseded_retention_days = excluded.superseded_retention_days, batch_limit = excluded.batch_limit",
            params![stage_id, base_revision_sql, now, revision_sql, desired.default_ttl_days(), desired.max_ttl_days(), desired.expiry_grace_days(), desired.superseded_retention_days(), desired.batch_limit()],
        )
        .map_err(|_| retention_error())?;
    Ok(Some(stage_id))
}

fn activate_staged_policy_transaction(
    transaction: &rusqlite::Transaction<'_>,
    desired: &RelationshipMemoryRetentionPolicy,
    stage_id: Option<&str>,
    now: i64,
) -> Result<(), MemoryStoreError> {
    let active = load_active_policy(transaction)?;
    if active.as_ref() == Some(desired) {
        return Ok(());
    }
    let Some(stage_id) = stage_id else {
        return Err(MemoryStoreError::Conflict);
    };
    let staged: Option<(String, Option<u64>, RelationshipMemoryRetentionPolicy)> = transaction
        .query_row(
            "SELECT stage_id, base_policy_revision, policy_revision, default_ttl_days, max_ttl_days, expiry_grace_days, superseded_retention_days, batch_limit FROM relationship_memory_retention_policy_stage WHERE singleton = 1",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get::<_, Option<i64>>(1)?
                        .map(u64::try_from)
                        .transpose()
                        .map_err(|_| rusqlite::Error::InvalidQuery)?,
                    RelationshipMemoryRetentionPolicy::new(
                        read_revision(row, 2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                    )
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
                ))
            },
        )
        .optional()
        .map_err(|_| retention_error())?;
    let expected_base = active
        .as_ref()
        .map(RelationshipMemoryRetentionPolicy::revision);
    let Some((actual_stage_id, actual_base, staged_policy)) = staged else {
        return Err(MemoryStoreError::Conflict);
    };
    if actual_stage_id != stage_id || actual_base != expected_base || staged_policy != *desired {
        return Err(MemoryStoreError::Conflict);
    }
    activate_policy_transaction(transaction, desired, now)?;
    if transaction
        .execute(
            "DELETE FROM relationship_memory_retention_policy_stage WHERE singleton = 1 AND stage_id = ?1",
            [stage_id],
        )
        .map_err(|_| retention_error())?
        != 1
    {
        return Err(MemoryStoreError::Conflict);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct EffectiveClock {
    now: i64,
    quarantined: bool,
}

fn resolve_effective_now(
    transaction: &rusqlite::Transaction<'_>,
    policy_revision: u64,
    wall_now: i64,
) -> Result<EffectiveClock, MemoryStoreError> {
    let (watermark, quarantined): (i64, Option<i64>) = transaction
        .query_row(
            "SELECT clock_watermark, quarantined_forward_time FROM relationship_memory_retention_state WHERE singleton = 1 AND policy_revision = ?1",
            [i64::try_from(policy_revision).map_err(|_| retention_error())?],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|_| retention_error())?;
    if wall_now <= watermark {
        return Ok(EffectiveClock {
            now: watermark,
            quarantined: quarantined.is_some(),
        });
    }
    if let Some(candidate) = quarantined {
        if wall_now > candidate {
            transaction
                .execute(
                    "UPDATE relationship_memory_retention_state SET quarantined_forward_time = ?1, quarantined_observed_at = ?1 WHERE singleton = 1",
                    [wall_now],
                )
                .map_err(|_| retention_error())?;
        }
        return Ok(EffectiveClock {
            now: watermark,
            quarantined: true,
        });
    }
    let forward = wall_now
        .checked_sub(watermark)
        .ok_or_else(retention_error)?;
    if forward > MAX_UNCONFIRMED_CLOCK_FORWARD_SECS {
        transaction
            .execute(
                "UPDATE relationship_memory_retention_state SET quarantined_forward_time = ?1, quarantined_observed_at = ?1 WHERE singleton = 1",
                [wall_now],
            )
            .map_err(|_| retention_error())?;
        return Ok(EffectiveClock {
            now: watermark,
            quarantined: true,
        });
    }
    transaction
        .execute(
            "UPDATE relationship_memory_retention_state SET clock_watermark = ?1 WHERE singleton = 1",
            [wall_now],
        )
        .map_err(|_| retention_error())?;
    Ok(EffectiveClock {
        now: wall_now,
        quarantined: false,
    })
}

fn append_maintenance_audit(
    transaction: &rusqlite::Transaction<'_>,
    record_key: &str,
    revision: u64,
    now: i64,
    superseded: bool,
) -> Result<(), MemoryStoreError> {
    let event_id = allocate_identifier(transaction, IdentifierNamespace::AuditEvent)
        .map_err(|_| retention_error())?;
    transaction.execute(
        "INSERT INTO relationship_memory_audit (event_id, occurred_at, operation, target_record_key, before_revision, after_revision, actor_kind, actor_user_id, actor_agent_id, session_id, trace_id, changed_mask) VALUES (?1, ?2, ?3, ?4, ?5, NULL, 'system_service', NULL, NULL, NULL, NULL, 0)",
        params![event_id, now, if superseded { "purge_superseded" } else { "purge_expired" }, record_key, i64::try_from(revision).map_err(|_| retention_error())?],
    ).map_err(|_| retention_error())?;
    Ok(())
}

fn append_chain_forget_audit(
    transaction: &rusqlite::Transaction<'_>,
    record_key: &str,
    revision: i64,
    now: i64,
) -> Result<(), MemoryStoreError> {
    let event_id = allocate_identifier(transaction, IdentifierNamespace::AuditEvent)
        .map_err(|_| forget_error())?;
    transaction
        .execute(
            "INSERT INTO relationship_memory_audit (event_id, occurred_at, operation, target_record_key, before_revision, after_revision, actor_kind, actor_user_id, actor_agent_id, session_id, trace_id, changed_mask) VALUES (?1, ?2, 'forget_chain', ?3, ?4, NULL, 'system_service', NULL, NULL, NULL, NULL, 0)",
            params![event_id, now, record_key, revision],
        )
        .map_err(|_| forget_error())?;
    Ok(())
}

fn insert_retention_ledgers(
    transaction: &rusqlite::Transaction<'_>,
    policy: &RelationshipMemoryRetentionPolicy,
    now: i64,
    report: MemoryPurgeReport,
) -> Result<(), MemoryStoreError> {
    let run_id = allocate_identifier(transaction, IdentifierNamespace::RetentionRun)
        .map_err(|_| retention_error())?;
    let batch_id = allocate_identifier(transaction, IdentifierNamespace::RetentionBatch)
        .map_err(|_| retention_error())?;
    transaction.execute(
        "INSERT INTO relationship_memory_retention_runs (run_id, started_at, completed_at, policy_revision, clock_watermark, expired_count, superseded_count) VALUES (?1, ?2, ?2, ?3, ?2, ?4, ?5)",
        params![run_id, now, i64::try_from(policy.revision()).map_err(|_| retention_error())?, report.expired_count, report.superseded_count],
    ).map_err(|_| retention_error())?;
    transaction.execute(
        "INSERT INTO relationship_memory_retention_batches (batch_id, run_id, occurred_at, attempted_limit, expired_count, superseded_count) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![batch_id, run_id, now, policy.batch_limit(), report.expired_count, report.superseded_count],
    ).map_err(|_| retention_error())?;
    Ok(())
}

fn component_objects_exist(
    transaction: &rusqlite::Transaction<'_>,
) -> Result<bool, MemoryStoreError> {
    transaction
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name IN ('relationship_memories', 'relationship_memories_search', 'relationship_memory_audit', 'relationship_memory_audit_no_update', 'relationship_memory_audit_no_delete'))",
            [],
            |row| row.get(0),
        )
        .map_err(|_| schema_error())
}

fn schema_error() -> MemoryStoreError {
    MemoryStoreError::Store("unsupported relationship memory schema".into())
}

fn decode_row(
    row: &rusqlite::Row<'_>,
    user_id: &UserId,
    agent_id: &AgentId,
) -> rusqlite::Result<MemoryEntry> {
    let importance: i64 = row.get(5)?;
    Ok(MemoryEntry {
        id: row.get(0)?,
        owner: MemoryOwner::Relationship {
            user_id: user_id.clone(),
            agent_id: agent_id.clone(),
        },
        kind: decode(row, 1)?,
        content: row.get(2)?,
        references: decode(row, 3)?,
        tags: decode(row, 4)?,
        importance: decode_importance(importance, 5)?,
        created_at: row.get(6)?,
        last_accessed: row.get(7)?,
        access_count: row.get(8)?,
        metadata: decode(row, 9)?,
        revision: read_revision(row, 10)?,
        updated_at: row.get(11)?,
        expires_at: row.get(12)?,
        superseded_by: row.get(13)?,
        provenance: MemoryProvenance {
            actor: parse_actor(row.get::<_, String>(14)?.as_str(), 14)?,
            user_id: row.get::<_, Option<String>>(15)?.map(UserId::new),
            agent_id: row.get::<_, Option<String>>(16)?.map(AgentId::new),
            session_id: row
                .get::<_, Option<String>>(17)?
                .map(sylvander_protocol::types::SessionId::new),
            trace_id: row.get(18)?,
            source: parse_source(row.get::<_, String>(19)?.as_str(), 19)?,
            trusted: row.get::<_, i64>(20)? == 1,
        },
        retention_policy_revision: read_revision(row, 21)?,
    })
}

fn select_active_record(
    transaction: &rusqlite::Transaction<'_>,
    user_id: &UserId,
    agent_id: &AgentId,
    id: &str,
    now: i64,
) -> Result<Option<(String, u64)>, MemoryStoreError> {
    transaction
        .query_row(
            "SELECT record_key, revision FROM relationship_memories WHERE owner_user = ?1 AND owner_agent = ?2 AND id = ?3 AND superseded_by_record_key IS NULL AND (expires_at IS NULL OR expires_at > ?4)",
            params![user_id.0, agent_id.0, id, now],
            |row| Ok((row.get(0)?, read_revision(row, 1)?)),
        )
        .optional()
        .map_err(|_| mutation_error())
}

fn insert_entry(
    transaction: &rusqlite::Transaction<'_>,
    record_key: &str,
    entry: &MemoryEntry,
) -> Result<(), MemoryStoreError> {
    let MemoryOwner::Relationship { user_id, agent_id } = &entry.owner else {
        return Err(MemoryStoreError::InvalidInput);
    };
    transaction.execute(
        "INSERT INTO relationship_memories (record_key, owner_user, owner_agent, id, kind_json, content, references_json, tags_json, importance, created_at, last_accessed, access_count, metadata_json, revision, updated_at, expires_at, superseded_by_record_key, origin_actor_kind, origin_user_id, origin_agent_id, origin_session_id, origin_trace_id, origin_source, provenance_trusted, retention_policy_revision, integrity_epoch, integrity_mac) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, NULL, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, 0, '')",
        params![record_key, user_id.0, agent_id.0, entry.id, encode(&entry.kind)?, entry.content, encode(&entry.references)?, encode(&entry.tags)?, importance_value(entry.importance), entry.created_at, entry.last_accessed, entry.access_count, encode(&entry.metadata)?, i64::try_from(entry.revision).map_err(|_| MemoryStoreError::InvalidInput)?, entry.updated_at, entry.expires_at, actor_value(entry.provenance.actor), option_id(entry.provenance.user_id.as_ref()), option_id(entry.provenance.agent_id.as_ref()), option_id(entry.provenance.session_id.as_ref()), entry.provenance.trace_id, source_value(entry.provenance.source), entry.provenance.trusted, i64::try_from(entry.retention_policy_revision).map_err(|_| MemoryStoreError::InvalidInput)?],
    ).map_err(store_error)?;
    Ok(())
}

fn encode(value: &impl serde::Serialize) -> Result<String, MemoryStoreError> {
    serde_json::to_string(value).map_err(|_| store_failure())
}

fn decode<T: DeserializeOwned>(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<T> {
    let value: String = row.get(index)?;
    serde_json::from_str(&value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

const fn importance_value(importance: Importance) -> i64 {
    match importance {
        Importance::Low => 0,
        Importance::Medium => 1,
        Importance::High => 2,
        Importance::Critical => 3,
    }
}

fn decode_importance(value: i64, index: usize) -> rusqlite::Result<Importance> {
    match value {
        0 => Ok(Importance::Low),
        1 => Ok(Importance::Medium),
        2 => Ok(Importance::High),
        3 => Ok(Importance::Critical),
        _ => Err(rusqlite::Error::IntegralValueOutOfRange(index, value)),
    }
}

fn read_revision(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(index)?;
    u64::try_from(value)
        .ok()
        .filter(|revision| *revision > 0)
        .ok_or(rusqlite::Error::IntegralValueOutOfRange(index, value))
}

const fn actor_value(actor: super::memory::MemoryActorKind) -> &'static str {
    match actor {
        super::memory::MemoryActorKind::Worker => "worker",
        super::memory::MemoryActorKind::Guardian => "guardian",
        super::memory::MemoryActorKind::SystemService => "system_service",
    }
}

fn parse_actor(value: &str, index: usize) -> rusqlite::Result<super::memory::MemoryActorKind> {
    match value {
        "worker" => Ok(super::memory::MemoryActorKind::Worker),
        "guardian" => Ok(super::memory::MemoryActorKind::Guardian),
        "system_service" => Ok(super::memory::MemoryActorKind::SystemService),
        _ => Err(invalid_text(index, "invalid memory actor")),
    }
}

const fn source_value(source: MemoryProvenanceSource) -> &'static str {
    match source {
        MemoryProvenanceSource::Runtime => "runtime",
    }
}

fn parse_source(value: &str, index: usize) -> rusqlite::Result<MemoryProvenanceSource> {
    match value {
        "runtime" => Ok(MemoryProvenanceSource::Runtime),
        _ => Err(invalid_text(index, "invalid memory provenance source")),
    }
}

fn invalid_text(index: usize, message: &'static str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        index,
        rusqlite::types::Type::Text,
        std::io::Error::new(std::io::ErrorKind::InvalidData, message).into(),
    )
}

fn option_id<T: std::fmt::Display>(value: Option<&T>) -> Option<String> {
    value.map(ToString::to_string)
}

#[derive(Clone, Copy)]
enum IdentifierNamespace<'a> {
    Memory {
        user_id: &'a UserId,
        agent_id: &'a AgentId,
    },
    RecordKey,
    AuditEvent,
    RetentionRun,
    RetentionBatch,
}

fn allocate_identifier(
    transaction: &rusqlite::Transaction<'_>,
    namespace: IdentifierNamespace<'_>,
) -> Result<String, MemoryStoreError> {
    allocate_identifier_with(transaction, namespace, || uuid::Uuid::new_v4().to_string())
}

fn allocate_identifier_with(
    transaction: &rusqlite::Transaction<'_>,
    namespace: IdentifierNamespace<'_>,
    mut candidate: impl FnMut() -> String,
) -> Result<String, MemoryStoreError> {
    for _ in 0..MAX_IDENTIFIER_ATTEMPTS {
        let candidate = candidate();
        let exists = match namespace {
            IdentifierNamespace::Memory { user_id, agent_id } => transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM relationship_memories WHERE owner_user = ?1 AND owner_agent = ?2 AND id = ?3)",
                params![&user_id.0, &agent_id.0, &candidate],
                |row| row.get::<_, bool>(0),
            ),
            IdentifierNamespace::RecordKey => transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM relationship_memories WHERE record_key = ?1)",
                [&candidate],
                |row| row.get::<_, bool>(0),
            ),
            IdentifierNamespace::AuditEvent => transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM relationship_memory_audit WHERE event_id = ?1)",
                [&candidate],
                |row| row.get::<_, bool>(0),
            ),
            IdentifierNamespace::RetentionRun => transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM relationship_memory_retention_runs WHERE run_id = ?1)",
                [&candidate],
                |row| row.get::<_, bool>(0),
            ),
            IdentifierNamespace::RetentionBatch => transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM relationship_memory_retention_batches WHERE batch_id = ?1)",
                [&candidate],
                |row| row.get::<_, bool>(0),
            ),
        }
        .map_err(store_error)?;
        if !exists {
            return Ok(candidate);
        }
    }
    Err(store_failure())
}

#[allow(clippy::too_many_arguments)]
fn append_audit(
    transaction: &rusqlite::Transaction<'_>,
    ctx: &MemoryExecutionContext,
    record_key: &str,
    operation: &str,
    before_revision: Option<u64>,
    after_revision: Option<u64>,
    occurred_at: i64,
    changed_mask: i64,
) -> Result<(), MemoryStoreError> {
    let before = before_revision
        .map(i64::try_from)
        .transpose()
        .map_err(|_| MemoryStoreError::InvalidInput)?;
    let after = after_revision
        .map(i64::try_from)
        .transpose()
        .map_err(|_| MemoryStoreError::InvalidInput)?;
    let event_id = allocate_identifier(transaction, IdentifierNamespace::AuditEvent)?;
    transaction
        .execute(
            "INSERT INTO relationship_memory_audit (event_id, occurred_at, operation, target_record_key, before_revision, after_revision, actor_kind, actor_user_id, actor_agent_id, session_id, trace_id, changed_mask) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![event_id, occurred_at, operation, record_key, before, after, actor_value(ctx.actor()), option_id(ctx.user_id()), option_id(ctx.agent_id()), option_id(ctx.session_id()), ctx.trace_id(), changed_mask],
        )
        .map_err(store_error)?;
    Ok(())
}

fn store_error(_: rusqlite::Error) -> MemoryStoreError {
    store_failure()
}
fn search_error(_: rusqlite::Error) -> MemoryStoreError {
    MemoryStoreError::Search("memory search failed".into())
}
fn delete_error(_: rusqlite::Error) -> MemoryStoreError {
    MemoryStoreError::Delete("memory delete failed".into())
}

fn store_failure() -> MemoryStoreError {
    MemoryStoreError::Store("memory store operation failed".into())
}

fn mutation_error() -> MemoryStoreError {
    MemoryStoreError::Store("memory mutation failed".into())
}

fn retention_error() -> MemoryStoreError {
    MemoryStoreError::Store("memory retention operation failed".into())
}

fn forget_error() -> MemoryStoreError {
    MemoryStoreError::Store("memory chain forget failed".into())
}

#[cfg(test)]
#[path = "memory_sqlite_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "memory_sqlite_v2_tests.rs"]
mod v2_tests;

#[cfg(test)]
#[path = "memory_sqlite_hardening_tests.rs"]
mod hardening_tests;

#[cfg(test)]
#[path = "memory_sqlite_policy_activation_tests.rs"]
mod policy_activation_tests;
#[cfg(test)]
#[path = "memory_sqlite_retention_tests.rs"]
mod retention_tests;

#[cfg(test)]
#[path = "memory_sqlite_clock_tests.rs"]
mod clock_tests;

#[cfg(test)]
#[path = "memory_sqlite_chain_forget_tests.rs"]
mod chain_forget_tests;
