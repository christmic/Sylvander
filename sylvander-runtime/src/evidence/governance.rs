//! Encrypted, scope-enforced storage for evidence content and generated artifacts.
//!
//! The normalized evidence tables intentionally retain queryable metadata. Any
//! content that may contain user data is stored here with authenticated
//! encryption, finite retention, exact-scope access, and an append-only audit
//! trail for export and deletion.

use std::fmt::Write as _;
use std::sync::Arc;

use async_trait::async_trait;
use ring::aead::{AES_256_GCM, Aad, LessSafeKey, Nonce, UnboundKey};
use ring::rand::{SecureRandom, SystemRandom};
use rusqlite::{OptionalExtension, Transaction, params};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sylvander_agent::mcp_stdio::{McpResultArtifact, McpResultArtifactSink};

use super::{EvidenceError, EvidenceStore};

const KEY_BYTES: usize = 32;
const NONCE_BYTES: usize = 12;
const KEY_CHECK_PLAINTEXT: &[u8] = b"sylvander-evidence-governance-v1";
const KEY_CHECK_AAD: &[u8] = b"sylvander:evidence:key-check:v1";
const MAX_IDENTIFIER_BYTES: usize = 200;
const MAX_SOURCE_BYTES: usize = 512;
const MAX_MEDIA_TYPE_BYTES: usize = 128;
const MAX_RECORD_BYTES: usize = 16 * 1024 * 1024;
const MAX_RECORDS_PER_OPERATION: usize = 1_000;
const RETENTION_AUDIT_USER: &str = "\u{1f}retention";

/// Deployment and user boundary applied to every governed operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceScope {
    pub tenant_id: String,
    pub user_id: String,
}

impl EvidenceScope {
    pub fn new(tenant_id: impl Into<String>, user_id: impl Into<String>) -> Self {
        Self {
            tenant_id: tenant_id.into(),
            user_id: user_id.into(),
        }
    }
}

/// Data handling class persisted as queryable metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceClassification {
    Operational,
    Internal,
    Confidential,
    Restricted,
}

impl EvidenceClassification {
    fn as_str(self) -> &'static str {
        match self {
            Self::Operational => "operational",
            Self::Internal => "internal",
            Self::Confidential => "confidential",
            Self::Restricted => "restricted",
        }
    }

    fn parse(value: &str) -> Result<Self, EvidenceError> {
        match value {
            "operational" => Ok(Self::Operational),
            "internal" => Ok(Self::Internal),
            "confidential" => Ok(Self::Confidential),
            "restricted" => Ok(Self::Restricted),
            _ => Err(EvidenceError::InvalidGovernedRecord),
        }
    }
}

/// Governed payload kind. Generated artifacts deliberately share the same
/// retention, encryption, export, and deletion implementation as run evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GovernedRecordKind {
    Event,
    Artifact,
}

impl GovernedRecordKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Event => "event",
            Self::Artifact => "artifact",
        }
    }

    fn parse(value: &str) -> Result<Self, EvidenceError> {
        match value {
            "event" => Ok(Self::Event),
            "artifact" => Ok(Self::Artifact),
            _ => Err(EvidenceError::InvalidGovernedRecord),
        }
    }
}

/// Secret material used for application-layer AES-256-GCM encryption.
///
/// The key secret must be either 32 raw bytes or 64 ASCII hexadecimal
/// characters. A database-bound encrypted marker makes a wrong key or key ID
/// fail at open rather than failing only when a record is exported.
pub struct EvidenceEncryption {
    key_id: String,
    key: [u8; KEY_BYTES],
}

impl std::fmt::Debug for EvidenceEncryption {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EvidenceEncryption")
            .field("key_id", &self.key_id)
            .field("key", &"[REDACTED]")
            .finish()
    }
}

impl Drop for EvidenceEncryption {
    fn drop(&mut self) {
        self.key.fill(0);
    }
}

impl EvidenceEncryption {
    pub fn from_secret(key_id: impl Into<String>, secret: &[u8]) -> Result<Self, EvidenceError> {
        let key_id = key_id.into();
        validate_text(&key_id, MAX_IDENTIFIER_BYTES)?;
        let key = if secret.len() == KEY_BYTES {
            secret
                .try_into()
                .map_err(|_| EvidenceError::InvalidEncryptionKey)?
        } else if secret.len() == KEY_BYTES * 2 {
            decode_hex_key(secret)?
        } else {
            return Err(EvidenceError::InvalidEncryptionKey);
        };
        Ok(Self { key_id, key })
    }
}

/// Finite policy bound to one database and one deployment tenant.
#[derive(Debug)]
pub struct EvidenceGovernance {
    pub tenant_id: String,
    pub retention_days: u32,
    pub encryption: EvidenceEncryption,
}

impl EvidenceGovernance {
    pub fn new(
        tenant_id: impl Into<String>,
        retention_days: u32,
        encryption: EvidenceEncryption,
    ) -> Result<Self, EvidenceError> {
        let tenant_id = tenant_id.into();
        validate_text(&tenant_id, MAX_IDENTIFIER_BYTES)?;
        if !(1..=3_650).contains(&retention_days) {
            return Err(EvidenceError::InvalidRetentionPolicy);
        }
        Ok(Self {
            tenant_id,
            retention_days,
            encryption,
        })
    }
}

pub(super) struct GovernanceState {
    tenant_id: String,
    retention_seconds: i64,
    key_id: String,
    key: LessSafeKey,
}

impl GovernanceState {
    fn new(policy: EvidenceGovernance) -> Result<Self, EvidenceError> {
        let key = UnboundKey::new(&AES_256_GCM, &policy.encryption.key)
            .map_err(|_| EvidenceError::InvalidEncryptionKey)?;
        Ok(Self {
            tenant_id: policy.tenant_id,
            retention_seconds: i64::from(policy.retention_days).saturating_mul(86_400),
            key_id: policy.encryption.key_id.clone(),
            key: LessSafeKey::new(key),
        })
    }

    fn scope(&self, user_id: impl Into<String>) -> EvidenceScope {
        EvidenceScope::new(self.tenant_id.clone(), user_id)
    }

    fn seal(&self, plaintext: &[u8], aad: &[u8]) -> Result<SealedPayload, EvidenceError> {
        let mut nonce = [0_u8; NONCE_BYTES];
        SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| EvidenceError::EncryptionFailed)?;
        let mut ciphertext = plaintext.to_vec();
        self.key
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(aad),
                &mut ciphertext,
            )
            .map_err(|_| EvidenceError::EncryptionFailed)?;
        Ok(SealedPayload {
            nonce: nonce.to_vec(),
            ciphertext,
        })
    }

    fn open(&self, nonce: &[u8], ciphertext: &[u8], aad: &[u8]) -> Result<Vec<u8>, EvidenceError> {
        let nonce: [u8; NONCE_BYTES] = nonce
            .try_into()
            .map_err(|_| EvidenceError::InvalidGovernedRecord)?;
        let mut plaintext = ciphertext.to_vec();
        let opened = self
            .key
            .open_in_place(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(aad),
                &mut plaintext,
            )
            .map_err(|_| EvidenceError::DecryptionFailed)?;
        let length = opened.len();
        plaintext.truncate(length);
        Ok(plaintext)
    }
}

struct SealedPayload {
    nonce: Vec<u8>,
    ciphertext: Vec<u8>,
}

/// Content or generated artifact to persist under one exact scope.
#[derive(Debug, Clone)]
pub struct GovernedRecordInput {
    pub id: String,
    pub scope: EvidenceScope,
    pub kind: GovernedRecordKind,
    pub classification: EvidenceClassification,
    pub source_ref: String,
    pub media_type: String,
    pub payload: Vec<u8>,
    pub created_at: i64,
}

/// Decrypted record returned only after exact tenant/user authorization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GovernedRecord {
    pub id: String,
    pub scope: EvidenceScope,
    pub kind: GovernedRecordKind,
    pub classification: EvidenceClassification,
    pub source_ref: String,
    pub media_type: String,
    pub payload: Vec<u8>,
    pub payload_digest_sha256: String,
    pub created_at: i64,
    pub expires_at: i64,
}

/// Deterministic export plus the durable audit entry that authorized it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceExport {
    pub audit_id: String,
    pub scope: EvidenceScope,
    pub records: Vec<GovernedRecord>,
    pub digest_sha256: String,
    pub exported_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GovernanceAudit {
    pub id: String,
    pub scope: EvidenceScope,
    pub action: String,
    pub selector_digest_sha256: String,
    pub result_digest_sha256: String,
    pub record_count: u64,
    pub occurred_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionSweep {
    pub audit_id: Option<String>,
    pub removed: u64,
}

/// Runtime adapter that routes complete MCP results into the same governed
/// store as run evidence instead of writing plaintext files below `data_dir`.
#[derive(Clone)]
pub struct EvidenceArtifactSink {
    store: EvidenceStore,
}

impl EvidenceArtifactSink {
    pub fn new(store: EvidenceStore) -> Result<Self, EvidenceError> {
        if !store.governance_enabled() {
            return Err(EvidenceError::EncryptionRequired);
        }
        Ok(Self { store })
    }
}

#[async_trait]
impl McpResultArtifactSink for EvidenceArtifactSink {
    async fn persist(&self, artifact: McpResultArtifact) -> Result<String, String> {
        let id = uuid::Uuid::new_v4().to_string();
        let source =
            bounded_artifact_source(&artifact.server, &artifact.operation, &artifact.session_id);
        self.store
            .put_governed_record(GovernedRecordInput {
                id: id.clone(),
                scope: self
                    .store
                    .governed_scope(artifact.user_id)
                    .map_err(|error| error.to_string())?,
                kind: GovernedRecordKind::Artifact,
                classification: EvidenceClassification::Restricted,
                source_ref: source,
                media_type: artifact.media_type,
                payload: artifact.payload,
                created_at: artifact.created_at,
            })
            .await
            .map_err(|error| error.to_string())?;
        Ok(format!("evidence-artifact:{id}"))
    }
}

impl EvidenceStore {
    /// Open a tenant-bound store whose governed payloads are encrypted.
    pub async fn open_governed(
        path: impl AsRef<std::path::Path>,
        governance: EvidenceGovernance,
    ) -> Result<Self, EvidenceError> {
        let path = path.as_ref().to_path_buf();
        let state = Arc::new(GovernanceState::new(governance)?);
        let store =
            Self::open_connection(move || rusqlite::Connection::open(path), Some(state)).await?;
        store
            .sweep_governed_retention(unix_timestamp(), 1_000)
            .await?;
        Ok(store)
    }

    #[cfg(test)]
    pub(crate) async fn open_governed_in_memory(
        governance: EvidenceGovernance,
    ) -> Result<Self, EvidenceError> {
        let state = Arc::new(GovernanceState::new(governance)?);
        Self::open_connection(rusqlite::Connection::open_in_memory, Some(state)).await
    }

    pub fn governed_scope(
        &self,
        user_id: impl Into<String>,
    ) -> Result<EvidenceScope, EvidenceError> {
        let state = self.governance()?;
        let scope = state.scope(user_id);
        validate_scope(&scope, state)?;
        Ok(scope)
    }

    pub fn governance_enabled(&self) -> bool {
        self.governance.is_some()
    }

    /// Encrypt and retain one evidence payload or generated artifact.
    pub async fn put_governed_record(
        &self,
        input: GovernedRecordInput,
    ) -> Result<(), EvidenceError> {
        let state = self.governance()?.clone();
        validate_input(&input, &state)?;
        let digest = sha256(&input.payload);
        let expires_at = input
            .created_at
            .checked_add(state.retention_seconds)
            .ok_or(EvidenceError::InvalidGovernedRecord)?;
        let aad = record_aad(
            &input.id,
            &input.scope,
            input.kind,
            input.classification,
            &input.source_ref,
            &input.media_type,
            &digest,
            input.created_at,
            expires_at,
            &state.key_id,
        );
        let sealed = state.seal(&input.payload, &aad)?;
        let key_id = state.key_id.clone();
        self.run(move |connection| {
            let tombstoned: bool = connection
                .query_row(
                    "SELECT EXISTS(
                       SELECT 1 FROM evidence_governance_tombstones
                       WHERE tenant_id=?1 AND user_id=?2 AND record_id=?3
                     )",
                    params![input.scope.tenant_id, input.scope.user_id, input.id],
                    |row| row.get(0),
                )
                .map_err(EvidenceError::sqlite)?;
            if tombstoned {
                return Err(EvidenceError::GovernedRecordDeleted);
            }
            connection
                .execute(
                    "INSERT INTO evidence_governed_records(
                       tenant_id,user_id,id,kind,classification,source_ref,media_type,
                       created_at,expires_at,payload_digest_sha256,payload_nonce,
                       payload_ciphertext,key_id
                     ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
                    params![
                        input.scope.tenant_id,
                        input.scope.user_id,
                        input.id,
                        input.kind.as_str(),
                        input.classification.as_str(),
                        input.source_ref,
                        input.media_type,
                        input.created_at,
                        expires_at,
                        digest,
                        sealed.nonce,
                        sealed.ciphertext,
                        key_id,
                    ],
                )
                .map_err(EvidenceError::sqlite)?;
            Ok(())
        })
        .await
    }

    /// Export an exact record set and append a durable export audit atomically.
    pub async fn export_governed_records(
        &self,
        scope: EvidenceScope,
        record_ids: Vec<String>,
        exported_at: i64,
    ) -> Result<EvidenceExport, EvidenceError> {
        let state = self.governance()?.clone();
        validate_scope(&scope, &state)?;
        let record_ids = canonical_record_ids(record_ids)?;
        let selector_digest = selector_digest(&record_ids);
        let audit_id = uuid::Uuid::new_v4().to_string();
        let audit_id_result = audit_id.clone();
        let scope_result = scope.clone();
        self.run(move |connection| {
            let transaction = connection
                .unchecked_transaction()
                .map_err(EvidenceError::sqlite)?;
            let mut records = Vec::with_capacity(record_ids.len());
            for record_id in &record_ids {
                let stored = load_stored(&transaction, &scope, record_id)?
                    .ok_or(EvidenceError::GovernedRecordNotFound)?;
                records.push(decrypt_record(&state, scope.clone(), stored)?);
            }
            let digest = export_digest(&scope, &records);
            insert_audit(
                &transaction,
                &GovernanceAudit {
                    id: audit_id.clone(),
                    scope: scope.clone(),
                    action: "export".into(),
                    selector_digest_sha256: selector_digest,
                    result_digest_sha256: digest.clone(),
                    record_count: records.len() as u64,
                    occurred_at: exported_at,
                },
            )?;
            transaction.commit().map_err(EvidenceError::sqlite)?;
            Ok((records, digest))
        })
        .await
        .map(|(records, digest_sha256)| EvidenceExport {
            audit_id: audit_id_result,
            scope: scope_result,
            records,
            digest_sha256,
            exported_at,
        })
    }

    /// Delete an exact record set inside one tenant/user scope. Ciphertext is
    /// physically removed while a content-free tombstone and audit remain.
    pub async fn delete_governed_records(
        &self,
        scope: EvidenceScope,
        record_ids: Vec<String>,
        reason: String,
        deleted_at: i64,
    ) -> Result<GovernanceAudit, EvidenceError> {
        let state = self.governance()?.clone();
        validate_scope(&scope, &state)?;
        validate_text(&reason, MAX_SOURCE_BYTES)?;
        let record_ids = canonical_record_ids(record_ids)?;
        let selector_digest = selector_digest(&record_ids);
        let audit = GovernanceAudit {
            id: uuid::Uuid::new_v4().to_string(),
            scope: scope.clone(),
            action: "delete".into(),
            selector_digest_sha256: selector_digest,
            result_digest_sha256: sha256(reason.as_bytes()),
            record_count: record_ids.len() as u64,
            occurred_at: deleted_at,
        };
        let audit_result = audit.clone();
        self.run(move |connection| {
            let transaction = connection
                .unchecked_transaction()
                .map_err(EvidenceError::sqlite)?;
            for record_id in &record_ids {
                let record = load_stored(&transaction, &scope, record_id)?
                    .ok_or(EvidenceError::GovernedRecordNotFound)?;
                insert_tombstone(&transaction, &scope, &record, &reason, deleted_at)?;
                let removed = transaction
                    .execute(
                        "DELETE FROM evidence_governed_records
                         WHERE tenant_id=?1 AND user_id=?2 AND id=?3",
                        params![scope.tenant_id, scope.user_id, record_id],
                    )
                    .map_err(EvidenceError::sqlite)?;
                if removed != 1 {
                    return Err(EvidenceError::GovernedRecordNotFound);
                }
            }
            insert_audit(&transaction, &audit)?;
            transaction.commit().map_err(EvidenceError::sqlite)
        })
        .await?;
        Ok(audit_result)
    }

    /// Remove expired evidence and artifacts under the bound tenant.
    pub async fn sweep_governed_retention(
        &self,
        now: i64,
        limit: u16,
    ) -> Result<RetentionSweep, EvidenceError> {
        let state = self.governance()?.clone();
        let limit = i64::from(if limit == 0 { 100 } else { limit.min(1_000) });
        let audit_id = uuid::Uuid::new_v4().to_string();
        let audit_id_result = audit_id.clone();
        let tenant_id = state.tenant_id.clone();
        let removed = self
            .run(move |connection| {
                let transaction = connection
                    .unchecked_transaction()
                    .map_err(EvidenceError::sqlite)?;
                let mut statement = transaction
                    .prepare(
                        "SELECT tenant_id,user_id,id,kind,classification,source_ref,media_type,
                                created_at,expires_at,payload_digest_sha256,payload_nonce,
                                payload_ciphertext,key_id
                         FROM evidence_governed_records
                         WHERE tenant_id=?1 AND expires_at<=?2
                         ORDER BY expires_at,user_id,id LIMIT ?3",
                    )
                    .map_err(EvidenceError::sqlite)?;
                let rows = statement
                    .query_map(params![tenant_id, now, limit], decode_stored_row)
                    .map_err(EvidenceError::sqlite)?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(EvidenceError::sqlite)?;
                drop(statement);
                for record in &rows {
                    let scope =
                        EvidenceScope::new(record.tenant_id.clone(), record.user_id.clone());
                    insert_tombstone(&transaction, &scope, record, "retention_expired", now)?;
                    transaction
                        .execute(
                            "DELETE FROM evidence_governed_records
                             WHERE tenant_id=?1 AND user_id=?2 AND id=?3",
                            params![record.tenant_id, record.user_id, record.id],
                        )
                        .map_err(EvidenceError::sqlite)?;
                }
                if !rows.is_empty() {
                    let selector = rows
                        .iter()
                        .map(|record| format!("{}\0{}", record.user_id, record.id))
                        .collect::<Vec<_>>();
                    insert_audit(
                        &transaction,
                        &GovernanceAudit {
                            id: audit_id,
                            scope: EvidenceScope::new(tenant_id, RETENTION_AUDIT_USER),
                            action: "retention".into(),
                            selector_digest_sha256: selector_digest(&selector),
                            result_digest_sha256: sha256(b"retention_expired"),
                            record_count: rows.len() as u64,
                            occurred_at: now,
                        },
                    )?;
                }
                transaction.commit().map_err(EvidenceError::sqlite)?;
                Ok(rows.len() as u64)
            })
            .await?;
        Ok(RetentionSweep {
            audit_id: (removed > 0).then_some(audit_id_result),
            removed,
        })
    }

    pub async fn governance_audits(
        &self,
        scope: EvidenceScope,
        limit: u16,
    ) -> Result<Vec<GovernanceAudit>, EvidenceError> {
        let state = self.governance()?.clone();
        validate_scope(&scope, &state)?;
        self.query_governance_audits(scope, limit).await
    }

    /// Return tenant-level retention audits. The stored pseudo-user contains a
    /// control byte that ordinary scope validation rejects, so no real user can
    /// collide with or query this stream through the user-scoped API.
    pub async fn retention_audits(
        &self,
        limit: u16,
    ) -> Result<Vec<GovernanceAudit>, EvidenceError> {
        let state = self.governance()?.clone();
        self.query_governance_audits(
            EvidenceScope::new(state.tenant_id.clone(), RETENTION_AUDIT_USER),
            limit,
        )
        .await
    }

    async fn query_governance_audits(
        &self,
        scope: EvidenceScope,
        limit: u16,
    ) -> Result<Vec<GovernanceAudit>, EvidenceError> {
        let limit = i64::from(if limit == 0 { 100 } else { limit.min(1_000) });
        self.run(move |connection| {
            let mut statement = connection
                .prepare(
                    "SELECT id,tenant_id,user_id,action,selector_digest_sha256,
                            result_digest_sha256,record_count,occurred_at
                     FROM evidence_governance_audit
                     WHERE tenant_id=?1 AND user_id=?2
                     ORDER BY sequence DESC LIMIT ?3",
                )
                .map_err(EvidenceError::sqlite)?;
            statement
                .query_map(params![scope.tenant_id, scope.user_id, limit], |row| {
                    Ok(GovernanceAudit {
                        id: row.get(0)?,
                        scope: EvidenceScope::new(
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                        ),
                        action: row.get(3)?,
                        selector_digest_sha256: row.get(4)?,
                        result_digest_sha256: row.get(5)?,
                        record_count: row
                            .get::<_, i64>(6)?
                            .try_into()
                            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(6, i64::MAX))?,
                        occurred_at: row.get(7)?,
                    })
                })
                .map_err(EvidenceError::sqlite)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(EvidenceError::sqlite)
        })
        .await
    }

    fn governance(&self) -> Result<&Arc<GovernanceState>, EvidenceError> {
        self.governance
            .as_ref()
            .ok_or(EvidenceError::EncryptionRequired)
    }
}

pub(super) fn initialize_governance(
    connection: &rusqlite::Connection,
    state: &GovernanceState,
) -> Result<(), EvidenceError> {
    validate_governance_schema(connection)?;
    let existing = connection
        .query_row(
            "SELECT tenant_id,key_id,key_check_nonce,key_check_ciphertext
             FROM evidence_governance_meta WHERE singleton=1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                ))
            },
        )
        .optional()
        .map_err(EvidenceError::sqlite)?;
    if let Some((tenant_id, key_id, nonce, ciphertext)) = existing {
        if tenant_id != state.tenant_id || key_id != state.key_id {
            return Err(EvidenceError::GovernanceBindingMismatch);
        }
        let plaintext = state.open(&nonce, &ciphertext, KEY_CHECK_AAD)?;
        if plaintext != KEY_CHECK_PLAINTEXT {
            return Err(EvidenceError::DecryptionFailed);
        }
        return Ok(());
    }
    let sealed = state.seal(KEY_CHECK_PLAINTEXT, KEY_CHECK_AAD)?;
    connection
        .execute(
            "INSERT INTO evidence_governance_meta(
               singleton,tenant_id,key_id,key_check_nonce,key_check_ciphertext
             ) VALUES (1,?1,?2,?3,?4)",
            params![
                state.tenant_id,
                state.key_id,
                sealed.nonce,
                sealed.ciphertext
            ],
        )
        .map_err(EvidenceError::sqlite)?;
    Ok(())
}

fn validate_governance_schema(connection: &rusqlite::Connection) -> Result<(), EvidenceError> {
    for (table, expected) in [
        (
            "evidence_governance_meta",
            &[
                "singleton",
                "tenant_id",
                "key_id",
                "key_check_nonce",
                "key_check_ciphertext",
            ][..],
        ),
        (
            "evidence_governed_records",
            &[
                "tenant_id",
                "user_id",
                "id",
                "kind",
                "classification",
                "source_ref",
                "media_type",
                "created_at",
                "expires_at",
                "payload_digest_sha256",
                "payload_nonce",
                "payload_ciphertext",
                "key_id",
            ][..],
        ),
        (
            "evidence_governance_tombstones",
            &[
                "tenant_id",
                "user_id",
                "record_id",
                "kind",
                "classification",
                "payload_digest_sha256",
                "deleted_at",
                "reason_digest_sha256",
            ][..],
        ),
        (
            "evidence_governance_audit",
            &[
                "sequence",
                "id",
                "tenant_id",
                "user_id",
                "action",
                "selector_digest_sha256",
                "result_digest_sha256",
                "record_count",
                "occurred_at",
            ][..],
        ),
    ] {
        let mut statement = connection
            .prepare(&format!("PRAGMA table_info({table})"))
            .map_err(EvidenceError::sqlite)?;
        let actual = statement
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(EvidenceError::sqlite)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(EvidenceError::sqlite)?;
        if actual
            .iter()
            .map(String::as_str)
            .ne(expected.iter().copied())
        {
            return Err(EvidenceError::InvalidGovernanceSchema);
        }
    }
    Ok(())
}

/// Preserve JSON shape while removing content-bearing and secret-bearing
/// values. This function never uses a textual regex over serialized JSON.
pub fn structured_redact(value: &Value) -> Value {
    redact_value(None, value)
}

fn redact_value(key: Option<&str>, value: &Value) -> Value {
    if key.is_some_and(is_sensitive_key) {
        return redact_sensitive_value(value);
    }
    match value {
        Value::Object(entries) => Value::Object(
            entries
                .iter()
                .map(|(key, value)| (key.clone(), redact_value(Some(key), value)))
                .collect(),
        ),
        Value::Array(values) => Value::Array(
            values
                .iter()
                .map(|value| redact_value(None, value))
                .collect(),
        ),
        _ => value.clone(),
    }
}

fn redact_sensitive_value(value: &Value) -> Value {
    match value {
        Value::Object(entries) => Value::Object(
            entries
                .iter()
                .map(|(key, value)| (key.clone(), redact_sensitive_value(value)))
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.iter().map(redact_sensitive_value).collect()),
        _ => Value::String("[REDACTED]".into()),
    }
}

fn is_sensitive_key(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "payload"
            | "text"
            | "data"
            | "content"
            | "authorization"
            | "cookie"
            | "password"
            | "secret"
            | "token"
            | "api_key"
            | "apikey"
            | "private_key"
    )
}

struct StoredRecord {
    tenant_id: String,
    user_id: String,
    id: String,
    kind: String,
    classification: String,
    source_ref: String,
    media_type: String,
    created_at: i64,
    expires_at: i64,
    digest: String,
    nonce: Vec<u8>,
    ciphertext: Vec<u8>,
    key_id: String,
}

fn load_stored(
    transaction: &Transaction<'_>,
    scope: &EvidenceScope,
    record_id: &str,
) -> Result<Option<StoredRecord>, EvidenceError> {
    transaction
        .query_row(
            "SELECT tenant_id,user_id,id,kind,classification,source_ref,media_type,
                    created_at,expires_at,payload_digest_sha256,payload_nonce,
                    payload_ciphertext,key_id
             FROM evidence_governed_records
             WHERE tenant_id=?1 AND user_id=?2 AND id=?3",
            params![scope.tenant_id, scope.user_id, record_id],
            decode_stored_row,
        )
        .optional()
        .map_err(EvidenceError::sqlite)
}

fn decode_stored_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredRecord> {
    Ok(StoredRecord {
        tenant_id: row.get(0)?,
        user_id: row.get(1)?,
        id: row.get(2)?,
        kind: row.get(3)?,
        classification: row.get(4)?,
        source_ref: row.get(5)?,
        media_type: row.get(6)?,
        created_at: row.get(7)?,
        expires_at: row.get(8)?,
        digest: row.get(9)?,
        nonce: row.get(10)?,
        ciphertext: row.get(11)?,
        key_id: row.get(12)?,
    })
}

fn decrypt_record(
    state: &GovernanceState,
    scope: EvidenceScope,
    stored: StoredRecord,
) -> Result<GovernedRecord, EvidenceError> {
    if stored.key_id != state.key_id {
        return Err(EvidenceError::GovernanceBindingMismatch);
    }
    let kind = GovernedRecordKind::parse(&stored.kind)?;
    let classification = EvidenceClassification::parse(&stored.classification)?;
    let aad = record_aad(
        &stored.id,
        &scope,
        kind,
        classification,
        &stored.source_ref,
        &stored.media_type,
        &stored.digest,
        stored.created_at,
        stored.expires_at,
        &stored.key_id,
    );
    let payload = state.open(&stored.nonce, &stored.ciphertext, &aad)?;
    if sha256(&payload) != stored.digest {
        return Err(EvidenceError::DecryptionFailed);
    }
    Ok(GovernedRecord {
        id: stored.id,
        scope,
        kind,
        classification,
        source_ref: stored.source_ref,
        media_type: stored.media_type,
        payload,
        payload_digest_sha256: stored.digest,
        created_at: stored.created_at,
        expires_at: stored.expires_at,
    })
}

fn insert_tombstone(
    transaction: &Transaction<'_>,
    scope: &EvidenceScope,
    record: &StoredRecord,
    reason: &str,
    deleted_at: i64,
) -> Result<(), EvidenceError> {
    transaction
        .execute(
            "INSERT INTO evidence_governance_tombstones(
               tenant_id,user_id,record_id,kind,classification,
               payload_digest_sha256,deleted_at,reason_digest_sha256
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                scope.tenant_id,
                scope.user_id,
                record.id,
                record.kind,
                record.classification,
                record.digest,
                deleted_at,
                sha256(reason.as_bytes())
            ],
        )
        .map_err(EvidenceError::sqlite)?;
    Ok(())
}

fn insert_audit(
    transaction: &Transaction<'_>,
    audit: &GovernanceAudit,
) -> Result<(), EvidenceError> {
    transaction
        .execute(
            "INSERT INTO evidence_governance_audit(
               id,tenant_id,user_id,action,selector_digest_sha256,
               result_digest_sha256,record_count,occurred_at
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                audit.id,
                audit.scope.tenant_id,
                audit.scope.user_id,
                audit.action,
                audit.selector_digest_sha256,
                audit.result_digest_sha256,
                i64::try_from(audit.record_count).map_err(|_| EvidenceError::CountTooLarge)?,
                audit.occurred_at
            ],
        )
        .map_err(EvidenceError::sqlite)?;
    Ok(())
}

fn validate_input(
    input: &GovernedRecordInput,
    state: &GovernanceState,
) -> Result<(), EvidenceError> {
    validate_scope(&input.scope, state)?;
    validate_text(&input.id, MAX_IDENTIFIER_BYTES)?;
    validate_text(&input.source_ref, MAX_SOURCE_BYTES)?;
    validate_text(&input.media_type, MAX_MEDIA_TYPE_BYTES)?;
    if input.created_at < 0 || input.payload.is_empty() || input.payload.len() > MAX_RECORD_BYTES {
        return Err(EvidenceError::InvalidGovernedRecord);
    }
    Ok(())
}

fn validate_scope(scope: &EvidenceScope, state: &GovernanceState) -> Result<(), EvidenceError> {
    validate_text(&scope.tenant_id, MAX_IDENTIFIER_BYTES)?;
    validate_text(&scope.user_id, MAX_IDENTIFIER_BYTES)?;
    if scope.tenant_id != state.tenant_id {
        return Err(EvidenceError::EvidenceScopeMismatch);
    }
    Ok(())
}

fn validate_text(value: &str, max_bytes: usize) -> Result<(), EvidenceError> {
    if value.is_empty()
        || value.len() > max_bytes
        || value.chars().any(char::is_control)
        || value.trim() != value
    {
        return Err(EvidenceError::InvalidGovernedRecord);
    }
    Ok(())
}

fn canonical_record_ids(mut record_ids: Vec<String>) -> Result<Vec<String>, EvidenceError> {
    if record_ids.is_empty() || record_ids.len() > MAX_RECORDS_PER_OPERATION {
        return Err(EvidenceError::InvalidGovernedRecord);
    }
    for record_id in &record_ids {
        validate_text(record_id, MAX_IDENTIFIER_BYTES)?;
    }
    record_ids.sort();
    if record_ids.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(EvidenceError::InvalidGovernedRecord);
    }
    Ok(record_ids)
}

#[allow(clippy::too_many_arguments)]
fn record_aad(
    id: &str,
    scope: &EvidenceScope,
    kind: GovernedRecordKind,
    classification: EvidenceClassification,
    source_ref: &str,
    media_type: &str,
    digest: &str,
    created_at: i64,
    expires_at: i64,
    key_id: &str,
) -> Vec<u8> {
    format!(
        "sylvander:evidence:record:v1\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}",
        scope.tenant_id,
        scope.user_id,
        id,
        kind.as_str(),
        classification.as_str(),
        source_ref,
        media_type,
        digest,
        created_at,
        expires_at,
        key_id
    )
    .into_bytes()
}

fn selector_digest(record_ids: &[String]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"sylvander:evidence:selector:v1\0");
    for record_id in record_ids {
        hasher.update(record_id.as_bytes());
        hasher.update(b"\0");
    }
    encode_digest(hasher.finalize())
}

fn bounded_artifact_source(server: &str, operation: &str, session_id: &str) -> String {
    let source = format!(
        "mcp:{server}:{operation}:session-sha256:{}",
        sha256(session_id.as_bytes())
    );
    if source.len() <= MAX_SOURCE_BYTES && !source.chars().any(char::is_control) {
        source
    } else {
        format!("mcp:sha256:{}", sha256(source.as_bytes()))
    }
}

fn export_digest(scope: &EvidenceScope, records: &[GovernedRecord]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"sylvander:evidence:export:v1\0");
    hasher.update(scope.tenant_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(scope.user_id.as_bytes());
    hasher.update(b"\0");
    for record in records {
        hasher.update(record.id.as_bytes());
        hasher.update(b"\0");
        hasher.update(record.payload_digest_sha256.as_bytes());
        hasher.update(b"\0");
        hasher.update(&record.payload);
        hasher.update(b"\0");
    }
    encode_digest(hasher.finalize())
}

fn sha256(bytes: &[u8]) -> String {
    encode_digest(Sha256::digest(bytes))
}

fn encode_digest(digest: impl AsRef<[u8]>) -> String {
    let bytes = digest.as_ref();
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn decode_hex_key(secret: &[u8]) -> Result<[u8; KEY_BYTES], EvidenceError> {
    let mut key = [0_u8; KEY_BYTES];
    for (index, pair) in secret.chunks_exact(2).enumerate() {
        let high = hex_nibble(pair[0]).ok_or(EvidenceError::InvalidEncryptionKey)?;
        let low = hex_nibble(pair[1]).ok_or(EvidenceError::InvalidEncryptionKey)?;
        key[index] = (high << 4) | low;
    }
    Ok(key)
}

fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn unix_timestamp() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .try_into()
        .unwrap_or(i64::MAX)
}

#[cfg(test)]
#[path = "../../tests/unit/evidence_governance.rs"]
mod tests;
