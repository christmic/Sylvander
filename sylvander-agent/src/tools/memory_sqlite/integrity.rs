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
    "SELECT record_key,owner_user,owner_agent,id,kind_json,content,references_json,tags_json,importance,created_at,last_accessed,access_count,metadata_json,revision,updated_at,expires_at,superseded_by_record_key,origin_actor_kind,origin_user_id,origin_agent_id,origin_session_id,origin_trace_id,origin_source,provenance_trusted,retention_policy_revision FROM relationship_memories ORDER BY record_key",
    "SELECT sequence,event_id,occurred_at,operation,target_record_key,before_revision,after_revision,actor_kind,actor_user_id,actor_agent_id,session_id,trace_id,changed_mask FROM relationship_memory_audit ORDER BY sequence",
    "SELECT singleton,clock_watermark,quarantined_forward_time,quarantined_observed_at,last_confirmed_forward_time,policy_revision,default_ttl_days,max_ttl_days,expiry_grace_days,superseded_retention_days,batch_limit FROM relationship_memory_retention_state ORDER BY singleton",
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
#[serde(deny_unknown_fields)]
pub(super) struct AnchorRecord {
    version: u32,
    schema_version: i64,
    epoch: u64,
    database_root: String,
    mac: String,
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
    pub(super) fn new(mut config: MemoryIntegrityConfig) -> Result<Self, MemoryStoreError> {
        let key = std::mem::take(&mut config.key);
        Ok(Self {
            anchor: config.anchor.clone(),
            key: Mutex::new(key),
        })
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
        if !constant_time_eq(actual.as_bytes(), record.database_root.as_bytes()) {
            return Err(integrity_error());
        }
        Ok(actual)
    }

    pub(super) fn seal_if_changed(
        &self,
        connection: &Connection,
        before: &str,
    ) -> Result<(), MemoryStoreError> {
        let after = database_root(connection)?;
        if constant_time_eq(before.as_bytes(), after.as_bytes()) {
            return Ok(());
        }
        let current = self.read_record()?;
        self.verify_record(&current)?;
        if !constant_time_eq(before.as_bytes(), current.database_root.as_bytes()) {
            return Err(integrity_error());
        }
        let epoch = current.epoch.checked_add(1).ok_or_else(integrity_error)?;
        self.write_record(epoch, after)
    }

    pub(super) fn snapshot(&self) -> Result<(u64, String), MemoryStoreError> {
        let record = self.read_record()?;
        self.verify_record(&record)?;
        Ok((record.epoch, record.database_root))
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
        if record.version != ANCHOR_VERSION
            || record.schema_version != SCHEMA_VERSION
            || record.epoch == 0
            || record.database_root.len() != 64
        {
            return Err(integrity_error());
        }
        self.verify_signature(&record_payload(record), &record.mac)
    }

    fn read_record(&self) -> Result<AnchorRecord, MemoryStoreError> {
        let metadata = std::fs::metadata(&self.anchor.path).map_err(|_| integrity_error())?;
        if !metadata.is_file() || metadata.len() > MAX_ANCHOR_BYTES {
            return Err(integrity_error());
        }
        serde_json::from_slice(&std::fs::read(&self.anchor.path).map_err(|_| integrity_error())?)
            .map_err(|_| integrity_error())
    }

    fn write_record(&self, epoch: u64, database_root: String) -> Result<(), MemoryStoreError> {
        self.write_record_impl(epoch, database_root, false)
    }

    fn write_new_record(&self, epoch: u64, database_root: String) -> Result<(), MemoryStoreError> {
        self.write_record_impl(epoch, database_root, true)
    }

    fn write_record_impl(
        &self,
        epoch: u64,
        database_root: String,
        create_new: bool,
    ) -> Result<(), MemoryStoreError> {
        let mut record = AnchorRecord {
            version: ANCHOR_VERSION,
            schema_version: SCHEMA_VERSION,
            epoch,
            database_root,
            mac: String::new(),
        };
        record.mac = self.sign(&record_payload(&record))?;
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

fn record_payload(record: &AnchorRecord) -> Vec<u8> {
    format!(
        "sylvander-memory-anchor-v1\n{}\n{}\n{}\n{}",
        record.version, record.schema_version, record.epoch, record.database_root
    )
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
