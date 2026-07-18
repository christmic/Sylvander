//! Durable, content-safe audit for credential lifecycle operations.
//!
//! The ledger is Runtime-owned and stored separately from the Agent/session
//! registry. Callers can persist only validated identities, fixed operation
//! classes, fixed result classes, and numeric revisions; secret references,
//! credential bytes, arbitrary errors, and caller-authored summaries have no
//! representation in this API.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;
use tokio::task;

const APPLICATION_ID: i64 = 0x5359_4341;
const SCHEMA_VERSION: i64 = 1;
const DEFAULT_RETENTION_SECS: i64 = 90 * 24 * 60 * 60;
const DEFAULT_PURGE_BATCH: u16 = 500;
const MAX_IDENTITY_BYTES: usize = 256;
const MAX_QUERY_LIMIT: u16 = 200;

const TABLE_SQL: &str = "CREATE TABLE credential_operation_audit (
  event_id TEXT PRIMARY KEY NOT NULL CHECK(length(event_id)=32),
  subject_kind TEXT NOT NULL CHECK(subject_kind IN ('channel_instance','provider','provider_binding')),
  subject_id TEXT NOT NULL CHECK(length(subject_id) BETWEEN 1 AND 256),
  binding_id_sha256 TEXT CHECK(binding_id_sha256 IS NULL OR length(binding_id_sha256)=64),
  operation TEXT NOT NULL CHECK(operation IN ('create','rotate','renew','revoke','failure')),
  credential_revision INTEGER CHECK(credential_revision IS NULL OR credential_revision > 0),
  occurred_at INTEGER NOT NULL CHECK(occurred_at >= 0),
  result_code TEXT NOT NULL CHECK(result_code IN ('succeeded','unavailable','invalid_request','invalid_lease','expired','missing_slot','invalid_encoding','registry_unavailable','integrity','conflict','storage_unavailable')),
  summary TEXT NOT NULL CHECK(summary IN ('credential operation completed','credential service unavailable','credential request rejected','credential lease rejected','credential lease expired','credential slot unavailable','credential encoding rejected','credential registry unavailable','credential integrity check failed','credential revision conflict','credential audit unavailable'))
) STRICT";
const INDEX_SQL: &str = "CREATE INDEX credential_operation_audit_subject_time
ON credential_operation_audit(subject_kind, subject_id, occurred_at DESC, event_id DESC)";

/// Runtime-validated audit identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CredentialAuditSubject {
    kind: &'static str,
    id: String,
    binding_id_sha256: Option<String>,
}

impl CredentialAuditSubject {
    /// Identify one independently configured Channel instance.
    pub fn channel_instance(instance_id: impl Into<String>) -> Result<Self, CredentialAuditError> {
        Self::new("channel_instance", instance_id.into(), None)
    }

    /// Identify one Provider and the credential binding it resolved.
    pub fn provider(
        provider_id: impl Into<String>,
        binding_id: &str,
    ) -> Result<Self, CredentialAuditError> {
        validate_identity(binding_id)?;
        Self::new(
            "provider",
            provider_id.into(),
            Some(sha256(binding_id.as_bytes())),
        )
    }

    /// Identify a Provider credential binding before its Provider is known.
    pub fn provider_binding(binding_id: &str) -> Result<Self, CredentialAuditError> {
        validate_identity(binding_id)?;
        let digest = sha256(binding_id.as_bytes());
        Self::new("provider_binding", digest.clone(), Some(digest))
    }

    fn new(
        kind: &'static str,
        id: String,
        binding_id_sha256: Option<String>,
    ) -> Result<Self, CredentialAuditError> {
        validate_identity(&id)?;
        Ok(Self {
            kind,
            id,
            binding_id_sha256,
        })
    }
}

/// Fixed credential lifecycle classes persisted by the ledger.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialAuditOperation {
    Create,
    Rotate,
    Renew,
    Revoke,
    Failure,
}

impl CredentialAuditOperation {
    const fn code(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Rotate => "rotate",
            Self::Renew => "renew",
            Self::Revoke => "revoke",
            Self::Failure => "failure",
        }
    }

    fn parse(value: &str) -> Result<Self, CredentialAuditError> {
        match value {
            "create" => Ok(Self::Create),
            "rotate" => Ok(Self::Rotate),
            "renew" => Ok(Self::Renew),
            "revoke" => Ok(Self::Revoke),
            "failure" => Ok(Self::Failure),
            _ => Err(CredentialAuditError::Integrity),
        }
    }
}

/// Fixed, content-safe result classes. Each maps to a constant summary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialAuditResult {
    Succeeded,
    Unavailable,
    InvalidRequest,
    InvalidLease,
    Expired,
    MissingSlot,
    InvalidEncoding,
    RegistryUnavailable,
    Integrity,
    Conflict,
    StorageUnavailable,
}

impl CredentialAuditResult {
    const fn code(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Unavailable => "unavailable",
            Self::InvalidRequest => "invalid_request",
            Self::InvalidLease => "invalid_lease",
            Self::Expired => "expired",
            Self::MissingSlot => "missing_slot",
            Self::InvalidEncoding => "invalid_encoding",
            Self::RegistryUnavailable => "registry_unavailable",
            Self::Integrity => "integrity",
            Self::Conflict => "conflict",
            Self::StorageUnavailable => "storage_unavailable",
        }
    }

    const fn summary(self) -> &'static str {
        match self {
            Self::Succeeded => "credential operation completed",
            Self::Unavailable => "credential service unavailable",
            Self::InvalidRequest => "credential request rejected",
            Self::InvalidLease => "credential lease rejected",
            Self::Expired => "credential lease expired",
            Self::MissingSlot => "credential slot unavailable",
            Self::InvalidEncoding => "credential encoding rejected",
            Self::RegistryUnavailable => "credential registry unavailable",
            Self::Integrity => "credential integrity check failed",
            Self::Conflict => "credential revision conflict",
            Self::StorageUnavailable => "credential audit unavailable",
        }
    }

    fn parse(value: &str) -> Result<Self, CredentialAuditError> {
        match value {
            "succeeded" => Ok(Self::Succeeded),
            "unavailable" => Ok(Self::Unavailable),
            "invalid_request" => Ok(Self::InvalidRequest),
            "invalid_lease" => Ok(Self::InvalidLease),
            "expired" => Ok(Self::Expired),
            "missing_slot" => Ok(Self::MissingSlot),
            "invalid_encoding" => Ok(Self::InvalidEncoding),
            "registry_unavailable" => Ok(Self::RegistryUnavailable),
            "integrity" => Ok(Self::Integrity),
            "conflict" => Ok(Self::Conflict),
            "storage_unavailable" => Ok(Self::StorageUnavailable),
            _ => Err(CredentialAuditError::Integrity),
        }
    }
}

/// One redacted persisted event returned by an identity-scoped query.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CredentialAuditEvent {
    pub event_id: String,
    pub operation: CredentialAuditOperation,
    pub credential_revision: Option<u64>,
    pub occurred_at_unix_secs: i64,
    pub result: CredentialAuditResult,
    pub summary: &'static str,
}

/// Exact-current `SQLite` credential audit ledger.
#[derive(Clone)]
pub struct CredentialOperationAuditLedger {
    connection: Arc<Mutex<Connection>>,
    policy: CredentialAuditRetention,
}

#[derive(Clone, Copy)]
struct CredentialAuditRetention {
    retention_secs: i64,
    purge_batch: u16,
}

impl Default for CredentialAuditRetention {
    fn default() -> Self {
        Self {
            retention_secs: DEFAULT_RETENTION_SECS,
            purge_batch: DEFAULT_PURGE_BATCH,
        }
    }
}

impl CredentialOperationAuditLedger {
    /// Open the latest schema. Old, partial, future, or foreign schemas fail closed.
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, CredentialAuditError> {
        let path = path.as_ref().to_path_buf();
        Self::open_connection(
            move || Connection::open(path),
            CredentialAuditRetention::default(),
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn open_in_memory_with_policy(
        retention_secs: i64,
        purge_batch: u16,
    ) -> Result<Self, CredentialAuditError> {
        if retention_secs <= 0 || purge_batch == 0 {
            return Err(CredentialAuditError::InvalidInput);
        }
        Self::open_connection(
            Connection::open_in_memory,
            CredentialAuditRetention {
                retention_secs,
                purge_batch,
            },
        )
        .await
    }

    async fn open_connection(
        open: impl FnOnce() -> rusqlite::Result<Connection> + Send + 'static,
        policy: CredentialAuditRetention,
    ) -> Result<Self, CredentialAuditError> {
        task::spawn_blocking(move || {
            let mut connection = open().map_err(storage)?;
            connection
                .busy_timeout(Duration::from_secs(5))
                .map_err(storage)?;
            initialize_or_validate_schema(&mut connection)?;
            let ledger = Self {
                connection: Arc::new(Mutex::new(connection)),
                policy,
            };
            Ok(ledger)
        })
        .await
        .map_err(|_| CredentialAuditError::Unavailable)?
    }

    /// Persist one operation. Successful credential issuance fails closed if
    /// this write cannot complete; callers decide how to map that safe error.
    pub async fn record(
        &self,
        subject: &CredentialAuditSubject,
        operation: CredentialAuditOperation,
        credential_revision: Option<u64>,
        result: CredentialAuditResult,
    ) -> Result<String, CredentialAuditError> {
        self.record_at(
            subject,
            operation,
            credential_revision,
            result,
            unix_timestamp(),
        )
        .await
    }

    async fn record_at(
        &self,
        subject: &CredentialAuditSubject,
        operation: CredentialAuditOperation,
        credential_revision: Option<u64>,
        result: CredentialAuditResult,
        occurred_at: i64,
    ) -> Result<String, CredentialAuditError> {
        if occurred_at < 0 || credential_revision == Some(0) {
            return Err(CredentialAuditError::InvalidInput);
        }
        let subject = subject.clone();
        let event_id = uuid::Uuid::new_v4().simple().to_string();
        let result_event_id = event_id.clone();
        let policy = self.policy;
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(storage)?;
            transaction
                .execute(
                    "INSERT INTO credential_operation_audit(event_id,subject_kind,subject_id,binding_id_sha256,operation,credential_revision,occurred_at,result_code,summary) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                    params![
                        event_id,
                        subject.kind,
                        subject.id,
                        subject.binding_id_sha256,
                        operation.code(),
                        credential_revision.map(sql_revision).transpose()?,
                        occurred_at,
                        result.code(),
                        result.summary(),
                    ],
                )
                .map_err(storage)?;
            purge_expired(&transaction, occurred_at, policy)?;
            transaction.commit().map_err(storage)
        })
        .await?;
        Ok(result_event_id)
    }

    /// Read one identity's newest events. Other Channel/Provider identities
    /// are never returned by this API.
    pub async fn list(
        &self,
        subject: &CredentialAuditSubject,
        limit: u16,
    ) -> Result<Vec<CredentialAuditEvent>, CredentialAuditError> {
        if !(1..=MAX_QUERY_LIMIT).contains(&limit) {
            return Err(CredentialAuditError::InvalidInput);
        }
        let subject = subject.clone();
        self.run(move |connection| {
            let mut statement = connection
                .prepare(
                    "SELECT event_id,operation,credential_revision,occurred_at,result_code,summary FROM credential_operation_audit WHERE subject_kind=?1 AND subject_id=?2 ORDER BY occurred_at DESC,rowid DESC LIMIT ?3",
                )
                .map_err(storage)?;
            let rows = statement
                .query_map(params![subject.kind, subject.id, i64::from(limit)], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                })
                .map_err(storage)?;
            rows.map(|row| {
                let (event_id, operation, revision, occurred_at, result, summary) =
                    row.map_err(storage)?;
                let operation = CredentialAuditOperation::parse(&operation)?;
                let result = CredentialAuditResult::parse(&result)?;
                if summary != result.summary() {
                    return Err(CredentialAuditError::Integrity);
                }
                Ok(CredentialAuditEvent {
                    event_id,
                    operation,
                    credential_revision: revision.map(decode_revision).transpose()?,
                    occurred_at_unix_secs: occurred_at,
                    result,
                    summary: result.summary(),
                })
            })
            .collect()
        })
        .await
    }

    async fn run<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Connection) -> Result<T, CredentialAuditError> + Send + 'static,
    ) -> Result<T, CredentialAuditError> {
        let connection = self.connection.clone();
        task::spawn_blocking(move || {
            let mut connection = connection.blocking_lock();
            validate_schema(&connection)?;
            operation(&mut connection)
        })
        .await
        .map_err(|_| CredentialAuditError::Unavailable)?
    }
}

impl std::fmt::Debug for CredentialOperationAuditLedger {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CredentialOperationAuditLedger")
            .finish_non_exhaustive()
    }
}

/// Public errors intentionally omit `SQLite` messages, paths, and stored values.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CredentialAuditError {
    #[error("credential audit input is invalid")]
    InvalidInput,
    #[error("credential audit schema is not current")]
    Schema,
    #[error("credential audit integrity check failed")]
    Integrity,
    #[error("credential audit is unavailable")]
    Unavailable,
}

fn initialize_or_validate_schema(connection: &mut Connection) -> Result<(), CredentialAuditError> {
    let objects = schema_objects(connection)?;
    let application_id: i64 = connection
        .query_row("PRAGMA application_id", [], |row| row.get(0))
        .map_err(storage)?;
    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(storage)?;
    if objects.is_empty() && application_id == 0 && version == 0 {
        let transaction = connection.transaction().map_err(storage)?;
        transaction.execute_batch(TABLE_SQL).map_err(storage)?;
        transaction.execute_batch(INDEX_SQL).map_err(storage)?;
        transaction
            .pragma_update(None, "application_id", APPLICATION_ID)
            .map_err(storage)?;
        transaction
            .pragma_update(None, "user_version", SCHEMA_VERSION)
            .map_err(storage)?;
        transaction.commit().map_err(storage)?;
    }
    validate_schema(connection)?;
    validate_database_integrity(connection)
}

fn validate_schema(connection: &Connection) -> Result<(), CredentialAuditError> {
    let application_id: i64 = connection
        .query_row("PRAGMA application_id", [], |row| row.get(0))
        .map_err(storage)?;
    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(storage)?;
    if application_id != APPLICATION_ID || version != SCHEMA_VERSION {
        return Err(CredentialAuditError::Schema);
    }
    let objects = schema_objects(connection)?;
    let expected = HashMap::from([
        (
            ("table".to_owned(), "credential_operation_audit".to_owned()),
            normalize_sql(TABLE_SQL),
        ),
        (
            (
                "index".to_owned(),
                "credential_operation_audit_subject_time".to_owned(),
            ),
            normalize_sql(INDEX_SQL),
        ),
    ]);
    if objects != expected {
        return Err(CredentialAuditError::Schema);
    }
    Ok(())
}

fn validate_database_integrity(connection: &Connection) -> Result<(), CredentialAuditError> {
    let violations: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM pragma_integrity_check WHERE integrity_check!='ok'",
            [],
            |row| row.get(0),
        )
        .map_err(storage)?;
    if violations != 0 {
        return Err(CredentialAuditError::Integrity);
    }
    Ok(())
}

fn schema_objects(
    connection: &Connection,
) -> Result<HashMap<(String, String), String>, CredentialAuditError> {
    let mut statement = connection
        .prepare("SELECT type,name,sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' ORDER BY type,name")
        .map_err(storage)?;
    statement
        .query_map([], |row| {
            Ok((
                (row.get::<_, String>(0)?, row.get::<_, String>(1)?),
                normalize_sql(&row.get::<_, String>(2)?),
            ))
        })
        .map_err(storage)?
        .map(|row| row.map_err(storage))
        .collect()
}

fn purge_expired(
    transaction: &rusqlite::Transaction<'_>,
    now: i64,
    policy: CredentialAuditRetention,
) -> Result<(), CredentialAuditError> {
    let cutoff = now.saturating_sub(policy.retention_secs);
    transaction
        .execute(
            "DELETE FROM credential_operation_audit WHERE event_id IN (SELECT event_id FROM credential_operation_audit WHERE occurred_at < ?1 ORDER BY occurred_at,event_id LIMIT ?2)",
            params![cutoff, i64::from(policy.purge_batch)],
        )
        .map_err(storage)?;
    Ok(())
}

fn normalize_sql(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn validate_identity(value: &str) -> Result<(), CredentialAuditError> {
    if value.is_empty()
        || value.len() > MAX_IDENTITY_BYTES
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return Err(CredentialAuditError::InvalidInput);
    }
    Ok(())
}

fn sql_revision(value: u64) -> Result<i64, CredentialAuditError> {
    i64::try_from(value).map_err(|_| CredentialAuditError::InvalidInput)
}

fn decode_revision(value: i64) -> Result<u64, CredentialAuditError> {
    u64::try_from(value)
        .ok()
        .filter(|value| *value > 0)
        .ok_or(CredentialAuditError::Integrity)
}

fn storage(_: rusqlite::Error) -> CredentialAuditError {
    CredentialAuditError::Unavailable
}

fn unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .try_into()
        .unwrap_or(i64::MAX)
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
#[path = "../tests/unit/credential_audit.rs"]
mod tests;
