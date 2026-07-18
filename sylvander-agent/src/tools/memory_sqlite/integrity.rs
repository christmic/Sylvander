use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use rusqlite::{Connection, types::ValueRef};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::integrity_anchor::{
    FileMemoryIntegrityAnchor, MemoryAnchorObservation, MonotonicMemoryAnchor,
};
use super::{MemoryStoreError, SCHEMA_VERSION};

const ANCHOR_VERSION: u32 = 1;
const INTEGRITY_ERROR: &str = "memory integrity verification failed";
const TABLE_QUERIES: &[&str] = &[
    "SELECT record_key,owner_user,owner_agent,id,kind_json,content,references_json,tags_json,importance,created_at,last_accessed,access_count,metadata_json,revision,updated_at,expires_at,superseded_by_record_key,origin_actor_kind,origin_user_id,origin_agent_id,origin_session_id,origin_trace_id,origin_source,provenance_trusted,retention_policy_revision,integrity_epoch,integrity_mac FROM relationship_memories ORDER BY record_key",
    "SELECT sequence,event_id,occurred_at,operation,target_record_key,before_revision,after_revision,actor_kind,actor_user_id,actor_agent_id,session_id,trace_id,changed_mask FROM relationship_memory_audit ORDER BY sequence",
    "SELECT singleton,policy_revision,default_ttl_days,max_ttl_days,expiry_grace_days,superseded_retention_days,batch_limit FROM relationship_memory_retention_state ORDER BY singleton",
    "SELECT singleton,stage_id,base_policy_revision,staged_at,policy_revision,default_ttl_days,max_ttl_days,expiry_grace_days,superseded_retention_days,batch_limit FROM relationship_memory_retention_policy_stage ORDER BY singleton",
    "SELECT run_id,started_at,completed_at,policy_revision,clock_watermark,expired_count,superseded_count FROM relationship_memory_retention_runs ORDER BY run_id",
    "SELECT batch_id,run_id,occurred_at,attempted_limit,expired_count,superseded_count FROM relationship_memory_retention_batches ORDER BY batch_id",
    "SELECT singleton,generation,checkpoint_epoch,checkpoint_root,checkpoint_sha256,audit_compacted_count,audit_summary_root,retention_compacted_count,retention_summary_root,updated_at FROM relationship_memory_checkpoint_state ORDER BY singleton",
];

pub struct MemoryIntegrityConfig {
    anchor: Arc<dyn MonotonicMemoryAnchor>,
    key: Vec<u8>,
}

impl MemoryIntegrityConfig {
    pub fn new(anchor_path: impl Into<PathBuf>, key: &[u8]) -> Result<Self, MemoryStoreError> {
        if key.len() < 32 || key.len() > 4096 {
            return Err(integrity_error());
        }
        Ok(Self {
            anchor: Arc::new(FileMemoryIntegrityAnchor::new(anchor_path)),
            key: key.to_vec(),
        })
    }

    pub fn with_anchor(
        anchor: Arc<dyn MonotonicMemoryAnchor>,
        key: &[u8],
    ) -> Result<Self, MemoryStoreError> {
        if key.len() < 32 || key.len() > 4096 {
            return Err(integrity_error());
        }
        Ok(Self {
            anchor,
            key: key.to_vec(),
        })
    }
}

impl Drop for MemoryIntegrityConfig {
    fn drop(&mut self) {
        self.key.fill(0);
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
    anchor: Arc<dyn MonotonicMemoryAnchor>,
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
            anchor: Arc::clone(&config.anchor),
            key: Mutex::new(key),
        }
    }

    pub(super) fn establish(&self, connection: &Connection) -> Result<(), MemoryStoreError> {
        if self.anchor.load().map_err(|_| integrity_error())?.is_some() {
            return Err(integrity_error());
        }
        let root = database_root(connection)?;
        self.write_new_record(1, root)
    }

    pub(super) fn verify(&self, connection: &Connection) -> Result<String, MemoryStoreError> {
        let observed = self.read_record()?;
        let record = &observed.record;
        self.verify_record(record)?;
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
                self.write_record(
                    &observed.observation,
                    AnchorRecord::committed(*epoch, root.clone(), self)?,
                )?;
            }
        }
        Ok(actual)
    }

    pub(super) fn prepare(&self, before: &str, after: &str) -> Result<(), MemoryStoreError> {
        let observed = self.read_record()?;
        self.verify_record(&observed.record)?;
        let AnchorRecord::Committed {
            epoch,
            database_root,
            ..
        } = &observed.record
        else {
            return Err(integrity_error());
        };
        if !constant_time_eq(before.as_bytes(), database_root.as_bytes()) {
            return Err(integrity_error());
        }
        let to_epoch = epoch.checked_add(1).ok_or_else(integrity_error)?;
        let pending = AnchorRecord::pending(*epoch, before, to_epoch, after, self)?;
        self.write_record(&observed.observation, pending)
    }

    pub(super) fn finalize(&self, after: &str) -> Result<(), MemoryStoreError> {
        let observed = self.read_record()?;
        self.verify_record(&observed.record)?;
        let AnchorRecord::Pending {
            to_epoch, to_root, ..
        } = &observed.record
        else {
            return Err(integrity_error());
        };
        if !constant_time_eq(after.as_bytes(), to_root.as_bytes()) {
            return Err(integrity_error());
        }
        self.write_record(
            &observed.observation,
            AnchorRecord::committed(*to_epoch, to_root.clone(), self)?,
        )
    }

    pub(super) fn snapshot(&self) -> Result<(u64, String), MemoryStoreError> {
        let observed = self.read_record()?;
        let record = observed.record;
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
        let observed = self.read_record()?;
        self.verify_record(&observed.record)?;
        if let AnchorRecord::Committed { epoch, .. } = observed.record {
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

    fn read_record(&self) -> Result<ObservedAnchorRecord, MemoryStoreError> {
        let observation = self
            .anchor
            .load()
            .map_err(|_| integrity_error())?
            .ok_or_else(integrity_error)?;
        let record = serde_json::from_slice(&observation.value).map_err(|_| integrity_error())?;
        Ok(ObservedAnchorRecord {
            observation,
            record,
        })
    }

    fn write_record(
        &self,
        current: &MemoryAnchorObservation,
        record: AnchorRecord,
    ) -> Result<(), MemoryStoreError> {
        let bytes = serde_json::to_vec(&record).map_err(|_| integrity_error())?;
        self.anchor
            .compare_and_swap(&current.revision, &bytes)
            .map(|_| ())
            .map_err(|_| integrity_error())
    }

    fn write_new_record(&self, epoch: u64, database_root: String) -> Result<(), MemoryStoreError> {
        let record = AnchorRecord::committed(epoch, database_root, self)?;
        let bytes = serde_json::to_vec(&record).map_err(|_| integrity_error())?;
        self.anchor
            .create(&bytes)
            .map(|_| ())
            .map_err(|_| integrity_error())
    }
}

struct ObservedAnchorRecord {
    observation: MemoryAnchorObservation,
    record: AnchorRecord,
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

fn integrity_error() -> MemoryStoreError {
    MemoryStoreError::Store(INTEGRITY_ERROR.into())
}

#[cfg(test)]
#[path = "../../../tests/unit/tools_memory_sqlite_integrity.rs"]
mod tests;
