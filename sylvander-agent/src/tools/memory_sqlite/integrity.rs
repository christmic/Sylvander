use std::fmt::Write as _;
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::{Connection, types::ValueRef};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{MemoryStoreError, SCHEMA_VERSION};

const ANCHOR_VERSION: u32 = 1;
const MAX_ANCHOR_BYTES: u64 = 8 * 1024;
const INTEGRITY_ERROR: &str = "memory integrity verification failed";
const TABLE_QUERIES: &[&str] = &[
    "SELECT record_key,owner_user,owner_agent,id,kind_json,content,references_json,tags_json,importance,created_at,last_accessed,access_count,metadata_json,revision,updated_at,expires_at,superseded_by_record_key,origin_actor_kind,origin_user_id,origin_agent_id,origin_session_id,origin_trace_id,origin_source,provenance_trusted,retention_policy_revision,integrity_epoch,integrity_mac FROM relationship_memories ORDER BY record_key",
    "SELECT sequence,event_id,occurred_at,operation,target_record_key,before_revision,after_revision,actor_kind,actor_user_id,actor_agent_id,session_id,trace_id,changed_mask FROM relationship_memory_audit ORDER BY sequence",
    "SELECT singleton,policy_revision,default_ttl_days,max_ttl_days,expiry_grace_days,superseded_retention_days,batch_limit FROM relationship_memory_retention_state ORDER BY singleton",
    "SELECT run_id,started_at,completed_at,policy_revision,clock_watermark,expired_count,superseded_count FROM relationship_memory_retention_runs ORDER BY run_id",
    "SELECT batch_id,run_id,occurred_at,attempted_limit,expired_count,superseded_count FROM relationship_memory_retention_batches ORDER BY batch_id",
];

pub struct MemoryIntegrityConfig {
    pub anchor: FileMemoryIntegrityAnchor,
    key: Vec<u8>,
}

impl MemoryIntegrityConfig {
    pub fn new(anchor_path: impl Into<PathBuf>, key: &[u8]) -> Result<Self, MemoryStoreError> {
        if key.len() < 32 || key.len() > 4096 {
            return Err(integrity_error());
        }
        Ok(Self {
            anchor: FileMemoryIntegrityAnchor::new(anchor_path),
            key: key.to_vec(),
        })
    }
}

impl Drop for MemoryIntegrityConfig {
    fn drop(&mut self) {
        self.key.fill(0);
    }
}

#[derive(Debug, Clone)]
pub struct FileMemoryIntegrityAnchor {
    path: PathBuf,
}

impl FileMemoryIntegrityAnchor {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum AnchorRecord {
    Committed {
        version: u32,
        schema_version: i64,
        epoch: u64,
        database_root: String,
        mac: String,
    },
    Pending {
        version: u32,
        schema_version: i64,
        from_epoch: u64,
        from_root: String,
        to_epoch: u64,
        to_root: String,
        mac: String,
    },
}

pub(super) struct IntegrityState {
    anchor: FileMemoryIntegrityAnchor,
    key: Mutex<Vec<u8>>,
}

impl Drop for IntegrityState {
    fn drop(&mut self) {
        if let Ok(mut key) = self.key.lock() {
            key.fill(0);
        }
    }
}

impl IntegrityState {
    pub(super) fn new(mut config: MemoryIntegrityConfig) -> Self {
        let key = std::mem::take(&mut config.key);
        Self {
            anchor: config.anchor.clone(),
            key: Mutex::new(key),
        }
    }

    pub(super) fn establish(&self, connection: &Connection) -> Result<(), MemoryStoreError> {
        if self.anchor.path.exists() {
            return Err(integrity_error());
        }
        let root = database_root(connection)?;
        self.write_new_record(1, root)
    }

    pub(super) fn verify(&self, connection: &Connection) -> Result<String, MemoryStoreError> {
        let record = self.read_record()?;
        self.verify_record(&record)?;
        let actual = database_root(connection)?;
        match record {
            AnchorRecord::Committed { database_root, .. } => {
                if !constant_time_eq(actual.as_bytes(), database_root.as_bytes()) {
                    return Err(integrity_error());
                }
            }
            AnchorRecord::Pending {
                from_epoch,
                from_root,
                to_epoch,
                to_root,
                ..
            } => {
                let (epoch, root) = if constant_time_eq(actual.as_bytes(), from_root.as_bytes()) {
                    (from_epoch, from_root)
                } else if constant_time_eq(actual.as_bytes(), to_root.as_bytes()) {
                    (to_epoch, to_root)
                } else {
                    return Err(integrity_error());
                };
                self.write_record(AnchorRecord::committed(epoch, root, self)?)?;
            }
        }
        Ok(actual)
    }

    pub(super) fn prepare(&self, before: &str, after: &str) -> Result<(), MemoryStoreError> {
        let current = self.read_record()?;
        self.verify_record(&current)?;
        let AnchorRecord::Committed {
            epoch,
            database_root,
            ..
        } = current
        else {
            return Err(integrity_error());
        };
        if !constant_time_eq(before.as_bytes(), database_root.as_bytes()) {
            return Err(integrity_error());
        }
        let to_epoch = epoch.checked_add(1).ok_or_else(integrity_error)?;
        let pending = AnchorRecord::pending(epoch, before, to_epoch, after, self)?;
        self.write_record(pending)
    }

    pub(super) fn finalize(&self, after: &str) -> Result<(), MemoryStoreError> {
        let current = self.read_record()?;
        self.verify_record(&current)?;
        let AnchorRecord::Pending {
            to_epoch, to_root, ..
        } = current
        else {
            return Err(integrity_error());
        };
        if !constant_time_eq(after.as_bytes(), to_root.as_bytes()) {
            return Err(integrity_error());
        }
        self.write_record(AnchorRecord::committed(to_epoch, to_root, self)?)
    }

    pub(super) fn snapshot(&self) -> Result<(u64, String), MemoryStoreError> {
        let record = self.read_record()?;
        self.verify_record(&record)?;
        let AnchorRecord::Committed {
            epoch,
            database_root,
            ..
        } = record
        else {
            return Err(integrity_error());
        };
        Ok((epoch, database_root))
    }

    pub(super) fn read_epoch(&self, connection: &Connection) -> Result<u64, MemoryStoreError> {
        let record = self.read_record()?;
        self.verify_record(&record)?;
        if let AnchorRecord::Committed { epoch, .. } = record {
            return Ok(epoch);
        }
        self.verify(connection)?;
        self.snapshot().map(|(epoch, _)| epoch)
    }

    pub(super) fn seal_rows(
        &self,
        transaction: &rusqlite::Transaction<'_>,
        epoch: u64,
    ) -> Result<(), MemoryStoreError> {
        let rows = {
            let mut statement = transaction
                .prepare("SELECT record_key,owner_user,owner_agent,id,kind_json,content,references_json,tags_json,importance,created_at,last_accessed,access_count,metadata_json,revision,updated_at,expires_at,superseded_by_record_key,origin_actor_kind,origin_user_id,origin_agent_id,origin_session_id,origin_trace_id,origin_source,provenance_trusted,retention_policy_revision FROM relationship_memories ORDER BY record_key")
                .map_err(|_| integrity_error())?;
            statement
                .query_map([], |row| {
                    let key: String = row.get(0)?;
                    let payload = row_payload(row, 0, 25, epoch)?;
                    Ok((key, payload))
                })
                .map_err(|_| integrity_error())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| integrity_error())?
        };
        for (record_key, payload) in rows {
            let mac = self.sign(&payload)?;
            transaction
                .execute(
                    "UPDATE relationship_memories SET integrity_epoch = ?1, integrity_mac = ?2 WHERE record_key = ?3",
                    rusqlite::params![i64::try_from(epoch).map_err(|_| integrity_error())?, mac, record_key],
                )
                .map_err(|_| integrity_error())?;
        }
        Ok(())
    }

    pub(super) fn verify_row(
        &self,
        row: &rusqlite::Row<'_>,
        start: usize,
        expected_epoch: u64,
    ) -> rusqlite::Result<()> {
        let epoch: i64 = row.get(start + 25)?;
        let signature: String = row.get(start + 26)?;
        if epoch != i64::try_from(expected_epoch).unwrap_or(-1) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let payload = row_payload(row, start, 25, expected_epoch)?;
        self.verify_signature(&payload, &signature)
            .map_err(|_| rusqlite::Error::InvalidQuery)
    }

    pub(super) fn sign(&self, payload: &[u8]) -> Result<String, MemoryStoreError> {
        let key = self.key.lock().map_err(|_| integrity_error())?;
        Ok(hex(&hmac_sha256(&key, payload)))
    }

    pub(super) fn verify_signature(
        &self,
        payload: &[u8],
        signature: &str,
    ) -> Result<(), MemoryStoreError> {
        let expected = self.sign(payload)?;
        if expected.len() != signature.len()
            || !constant_time_eq(expected.as_bytes(), signature.as_bytes())
        {
            return Err(integrity_error());
        }
        Ok(())
    }

    fn verify_record(&self, record: &AnchorRecord) -> Result<(), MemoryStoreError> {
        if !record.valid_shape() {
            return Err(integrity_error());
        }
        self.verify_signature(&record_payload(record), record.mac())
    }

    fn read_record(&self) -> Result<AnchorRecord, MemoryStoreError> {
        let metadata = std::fs::metadata(&self.anchor.path).map_err(|_| integrity_error())?;
        if !metadata.is_file() || metadata.len() > MAX_ANCHOR_BYTES {
            return Err(integrity_error());
        }
        serde_json::from_slice(&std::fs::read(&self.anchor.path).map_err(|_| integrity_error())?)
            .map_err(|_| integrity_error())
    }

    fn write_record(&self, record: AnchorRecord) -> Result<(), MemoryStoreError> {
        self.write_record_impl(record, false)
    }

    fn write_new_record(&self, epoch: u64, database_root: String) -> Result<(), MemoryStoreError> {
        let record = AnchorRecord::committed(epoch, database_root, self)?;
        self.write_record_impl(record, true)
    }

    fn write_record_impl(
        &self,
        record: AnchorRecord,
        create_new: bool,
    ) -> Result<(), MemoryStoreError> {
        let parent = self
            .anchor
            .path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .ok_or_else(integrity_error)?;
        std::fs::create_dir_all(parent).map_err(|_| integrity_error())?;
        let temp = parent.join(format!(".memory-anchor-{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| {
            let bytes = serde_json::to_vec(&record).map_err(|_| integrity_error())?;
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)
                .map_err(|_| integrity_error())?;
            file.write_all(&bytes).map_err(|_| integrity_error())?;
            file.sync_all().map_err(|_| integrity_error())?;
            secure_file(&temp)?;
            if create_new {
                std::fs::hard_link(&temp, &self.anchor.path).map_err(|_| integrity_error())?;
                std::fs::remove_file(&temp).map_err(|_| integrity_error())?;
            } else {
                std::fs::rename(&temp, &self.anchor.path).map_err(|_| integrity_error())?;
            }
            File::open(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(|_| integrity_error())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(temp);
        }
        result
    }
}

fn row_payload(
    row: &rusqlite::Row<'_>,
    start: usize,
    count: usize,
    epoch: u64,
) -> rusqlite::Result<Vec<u8>> {
    let mut digest = Sha256::new();
    digest.update(b"sylvander-memory-row-v1\0");
    digest.update(epoch.to_be_bytes());
    for index in start..start + count {
        match row.get_ref(index)? {
            ValueRef::Null => digest.update(b"N"),
            ValueRef::Integer(value) => {
                digest.update(b"I");
                digest.update(value.to_be_bytes());
            }
            ValueRef::Real(value) => {
                digest.update(b"F");
                digest.update(value.to_bits().to_be_bytes());
            }
            ValueRef::Text(value) => {
                digest.update(b"T");
                hash_field(&mut digest, value);
            }
            ValueRef::Blob(value) => {
                digest.update(b"B");
                hash_field(&mut digest, value);
            }
        }
    }
    Ok(digest.finalize().to_vec())
}

impl AnchorRecord {
    fn committed(
        epoch: u64,
        database_root: String,
        integrity: &IntegrityState,
    ) -> Result<Self, MemoryStoreError> {
        let mut record = Self::Committed {
            version: ANCHOR_VERSION,
            schema_version: SCHEMA_VERSION,
            epoch,
            database_root,
            mac: String::new(),
        };
        *record.mac_mut() = integrity.sign(&record_payload(&record))?;
        Ok(record)
    }

    fn pending(
        from_epoch: u64,
        from_root: &str,
        to_epoch: u64,
        to_root: &str,
        integrity: &IntegrityState,
    ) -> Result<Self, MemoryStoreError> {
        let mut record = Self::Pending {
            version: ANCHOR_VERSION,
            schema_version: SCHEMA_VERSION,
            from_epoch,
            from_root: from_root.into(),
            to_epoch,
            to_root: to_root.into(),
            mac: String::new(),
        };
        *record.mac_mut() = integrity.sign(&record_payload(&record))?;
        Ok(record)
    }

    fn mac(&self) -> &str {
        match self {
            Self::Committed { mac, .. } | Self::Pending { mac, .. } => mac,
        }
    }

    fn mac_mut(&mut self) -> &mut String {
        match self {
            Self::Committed { mac, .. } | Self::Pending { mac, .. } => mac,
        }
    }

    fn valid_shape(&self) -> bool {
        match self {
            Self::Committed {
                version,
                schema_version,
                epoch,
                database_root,
                ..
            } => {
                *version == ANCHOR_VERSION
                    && *schema_version == SCHEMA_VERSION
                    && *epoch > 0
                    && database_root.len() == 64
            }
            Self::Pending {
                version,
                schema_version,
                from_epoch,
                from_root,
                to_epoch,
                to_root,
                ..
            } => {
                *version == ANCHOR_VERSION
                    && *schema_version == SCHEMA_VERSION
                    && *from_epoch > 0
                    && *to_epoch == from_epoch.saturating_add(1)
                    && from_root.len() == 64
                    && to_root.len() == 64
            }
        }
    }
}

fn record_payload(record: &AnchorRecord) -> Vec<u8> {
    match record {
        AnchorRecord::Committed {
            version,
            schema_version,
            epoch,
            database_root,
            ..
        } => format!(
            "sylvander-memory-anchor-v1\ncommitted\n{version}\n{schema_version}\n{epoch}\n{database_root}"
        ),
        AnchorRecord::Pending {
            version,
            schema_version,
            from_epoch,
            from_root,
            to_epoch,
            to_root,
            ..
        } => format!(
            "sylvander-memory-anchor-v1\npending\n{version}\n{schema_version}\n{from_epoch}\n{from_root}\n{to_epoch}\n{to_root}"
        ),
    }
    .into_bytes()
}

pub(super) fn database_root(connection: &Connection) -> Result<String, MemoryStoreError> {
    let mut digest = Sha256::new();
    digest.update(b"sylvander-memory-database-root-v1\0");
    for query in TABLE_QUERIES {
        hash_field(&mut digest, query.as_bytes());
        let mut statement = connection.prepare(query).map_err(|_| integrity_error())?;
        let column_count = statement.column_count();
        let mut rows = statement.query([]).map_err(|_| integrity_error())?;
        while let Some(row) = rows.next().map_err(|_| integrity_error())? {
            digest.update(b"R");
            for index in 0..column_count {
                match row.get_ref(index).map_err(|_| integrity_error())? {
                    ValueRef::Null => digest.update(b"N"),
                    ValueRef::Integer(value) => {
                        digest.update(b"I");
                        digest.update(value.to_be_bytes());
                    }
                    ValueRef::Real(value) => {
                        digest.update(b"F");
                        digest.update(value.to_bits().to_be_bytes());
                    }
                    ValueRef::Text(value) => {
                        digest.update(b"T");
                        hash_field(&mut digest, value);
                    }
                    ValueRef::Blob(value) => {
                        digest.update(b"B");
                        hash_field(&mut digest, value);
                    }
                }
            }
        }
    }
    Ok(hex(&digest.finalize()))
}

fn hash_field(digest: &mut Sha256, value: &[u8]) {
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value);
}

fn hmac_sha256(key: &[u8], payload: &[u8]) -> [u8; 32] {
    let mut block = [0_u8; 64];
    if key.len() > block.len() {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut outer_pad = [0x5c_u8; 64];
    let mut inner_pad = [0x36_u8; 64];
    for index in 0..64 {
        outer_pad[index] ^= block[index];
        inner_pad[index] ^= block[index];
    }
    let inner = Sha256::new()
        .chain_update(inner_pad)
        .chain_update(payload)
        .finalize();
    Sha256::new()
        .chain_update(outer_pad)
        .chain_update(inner)
        .finalize()
        .into()
}

fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(&mut output, "{byte:02x}");
    }
    output
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

#[cfg(unix)]
fn secure_file(path: &Path) -> Result<(), MemoryStoreError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|_| integrity_error())
}

#[cfg(not(unix))]
fn secure_file(_: &Path) -> Result<(), MemoryStoreError> {
    Ok(())
}

fn integrity_error() -> MemoryStoreError {
    MemoryStoreError::Store(INTEGRITY_ERROR.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::memory::{MemoryAppend, MemoryExecutionContext, MemoryFilter, MemoryStore};
    use crate::tools::memory_sqlite::{RelationshipMemoryRetentionPolicy, SqliteMemoryStore};
    use sylvander_protocol::SessionContext;

    const KEY: &[u8] = b"0123456789abcdef0123456789abcdef";

    fn config(path: &Path) -> MemoryIntegrityConfig {
        MemoryIntegrityConfig::new(path, KEY).unwrap()
    }

    fn worker() -> MemoryExecutionContext {
        MemoryExecutionContext::application_worker(&SessionContext::new(
            "alice",
            "agent-a",
            "session-a",
        ))
    }

    fn open(database: &Path, anchor: &Path) -> Result<SqliteMemoryStore, MemoryStoreError> {
        SqliteMemoryStore::open_with_integrity(
            database,
            RelationshipMemoryRetentionPolicy::default(),
            config(anchor),
        )
    }

    #[tokio::test]
    async fn authenticated_anchor_survives_mutation_and_restart() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("memory.db");
        let anchor = directory.path().join("anchor/state.json");
        let store = open(&database, &anchor).unwrap();
        store
            .append_relationship(&worker(), MemoryAppend::new("durable"))
            .await
            .unwrap();
        let anchored = std::fs::read(&anchor).unwrap();
        store
            .search_relationship(&worker(), "durable", MemoryFilter::default())
            .await
            .unwrap();
        assert_eq!(std::fs::read(&anchor).unwrap(), anchored);
        drop(store);
        open(&database, &anchor).unwrap();
        let record: AnchorRecord =
            serde_json::from_slice(&std::fs::read(&anchor).unwrap()).unwrap();
        assert!(matches!(record, AnchorRecord::Committed { epoch, .. } if epoch > 1));
        let encoded = std::fs::read_to_string(anchor).unwrap();
        assert!(!encoded.contains(std::str::from_utf8(KEY).unwrap()));
        assert!(!encoded.contains("durable"));
    }

    #[tokio::test]
    async fn row_tamper_audit_deletion_and_database_rollback_fail_closed() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("memory.db");
        let anchor = directory.path().join("anchor.json");
        let store = open(&database, &anchor).unwrap();
        store
            .append_relationship(&worker(), MemoryAppend::new("first"))
            .await
            .unwrap();
        drop(store);
        let old_database = std::fs::read(&database).unwrap();

        let store = open(&database, &anchor).unwrap();
        store
            .append_relationship(&worker(), MemoryAppend::new("second"))
            .await
            .unwrap();
        drop(store);

        std::fs::write(&database, &old_database).unwrap();
        let error = open(&database, &anchor).unwrap_err();
        assert_eq!(
            error.to_string(),
            "store error: memory integrity verification failed"
        );

        // Restore the current database by making a fresh protected fixture,
        // then simulate direct row tampering that leaves the schema intact.
        let database = directory.path().join("tampered.db");
        let anchor = directory.path().join("tampered.anchor");
        let store = open(&database, &anchor).unwrap();
        store
            .append_relationship(&worker(), MemoryAppend::new("original"))
            .await
            .unwrap();
        drop(store);
        let connection = Connection::open(&database).unwrap();
        connection
            .execute("UPDATE relationship_memories SET content = 'forged'", [])
            .unwrap();
        drop(connection);
        let error = open(&database, &anchor).unwrap_err();
        assert_eq!(
            error.to_string(),
            "store error: memory integrity verification failed"
        );

        // Audit deletion requires removing the append-only trigger. Exact
        // schema verification rejects that attack before content is exposed.
        let database = directory.path().join("audit-tampered.db");
        let anchor = directory.path().join("audit-tampered.anchor");
        let store = open(&database, &anchor).unwrap();
        store
            .append_relationship(&worker(), MemoryAppend::new("original"))
            .await
            .unwrap();
        drop(store);
        let connection = Connection::open(&database).unwrap();
        connection
            .execute_batch(
                "DROP TRIGGER relationship_memory_audit_no_delete;\
                 DELETE FROM relationship_memory_audit;",
            )
            .unwrap();
        drop(connection);
        let error = open(&database, &anchor).unwrap_err();
        assert_eq!(
            error.to_string(),
            "store error: unsupported relationship memory schema"
        );
    }

    #[tokio::test]
    async fn live_recall_rejects_forged_row_without_scanning_or_resealing_anchor() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("memory.db");
        let anchor = directory.path().join("anchor.json");
        let store = open(&database, &anchor).unwrap();
        store
            .append_relationship(&worker(), MemoryAppend::new("trusted"))
            .await
            .unwrap();
        let anchored = std::fs::read(&anchor).unwrap();
        let connection = Connection::open(&database).unwrap();
        connection
            .execute("UPDATE relationship_memories SET content = 'forged'", [])
            .unwrap();
        drop(connection);

        let error = store
            .search_relationship(&worker(), "", MemoryFilter::default())
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "search error: memory search failed");
        assert_eq!(std::fs::read(anchor).unwrap(), anchored);
    }

    #[test]
    fn missing_modified_or_wrong_key_anchor_is_rejected_without_secret_disclosure() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("memory.db");
        let anchor = directory.path().join("anchor.json");
        drop(open(&database, &anchor).unwrap());

        let wrong =
            MemoryIntegrityConfig::new(&anchor, b"abcdef0123456789abcdef0123456789").unwrap();
        let error = SqliteMemoryStore::open_with_integrity(
            &database,
            RelationshipMemoryRetentionPolicy::default(),
            wrong,
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "store error: memory integrity verification failed"
        );
        assert!(!format!("{error:?}").contains("abcdef"));

        let mut record: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&anchor).unwrap()).unwrap();
        record["epoch"] = serde_json::json!(999);
        std::fs::write(&anchor, serde_json::to_vec(&record).unwrap()).unwrap();
        assert!(open(&database, &anchor).is_err());
        std::fs::remove_file(&anchor).unwrap();
        assert!(open(&database, &anchor).is_err());
    }

    #[tokio::test]
    async fn pending_anchor_recovers_only_prepared_rollback_or_commit_roots() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("memory.db");
        let anchor = directory.path().join("anchor.json");
        let store = open(&database, &anchor).unwrap();
        store
            .append_relationship(&worker(), MemoryAppend::new("before"))
            .await
            .unwrap();
        drop(store);

        let integrity = IntegrityState::new(config(&anchor));
        let mut connection = Connection::open(&database).unwrap();
        let before = integrity.verify(&connection).unwrap();
        let transaction = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        transaction
            .execute("UPDATE relationship_memories SET content = 'after'", [])
            .unwrap();
        let after = database_root(&transaction).unwrap();
        integrity.prepare(&before, &after).unwrap();
        transaction.commit().unwrap();
        drop(connection);
        drop(integrity);

        // Simulates a crash after SQLite commit and before finalize.
        drop(open(&database, &anchor).unwrap());
        let record: AnchorRecord =
            serde_json::from_slice(&std::fs::read(&anchor).unwrap()).unwrap();
        assert!(matches!(record, AnchorRecord::Committed { epoch, .. } if epoch > 1));

        let integrity = IntegrityState::new(config(&anchor));
        let mut connection = Connection::open(&database).unwrap();
        let before = integrity.verify(&connection).unwrap();
        let transaction = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        transaction
            .execute(
                "UPDATE relationship_memories SET content = 'rolled-back'",
                [],
            )
            .unwrap();
        let after = database_root(&transaction).unwrap();
        integrity.prepare(&before, &after).unwrap();
        transaction.rollback().unwrap();
        drop(connection);
        drop(integrity);

        // Simulates a crash after prepare and before SQLite commit.
        drop(open(&database, &anchor).unwrap());
    }
}
