use std::fmt::Write as FmtWrite;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write as IoWrite};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, MAIN_DB, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{MemoryStoreError, SCHEMA_VERSION, SqliteMemoryStore, verify_schema};

const MANIFEST_VERSION: u32 = 1;
const MAX_MANIFEST_BYTES: u64 = 16 * 1024;
const MIN_RETAINED_COPIES: u32 = 2;
const MAX_RETAINED_COPIES: u32 = 30;
const BACKUP_PREFIX: &str = "relationship-memory-";
const DATABASE_SUFFIX: &str = ".sqlite3";
const MANIFEST_SUFFIX: &str = ".manifest.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryBackupManifest {
    pub manifest_version: u32,
    pub schema_version: i64,
    pub created_at: i64,
    pub size_bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryBackupArtifact {
    pub database_path: PathBuf,
    pub manifest_path: PathBuf,
    pub manifest: MemoryBackupManifest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MemoryRestoreError {
    #[error("memory restore rejected")]
    Rejected,
    #[error("memory restore outcome requires operator inspection")]
    OutcomeUnknown,
}

/// Explicit offline administration surface. Restore must run before the live
/// database is opened by a Runtime or [`SqliteMemoryStore`].
#[derive(Debug, Clone, Copy, Default)]
pub struct SqliteMemoryAdmin;

impl SqliteMemoryAdmin {
    pub fn restore_offline(
        live_database: impl AsRef<Path>,
        backup_database: impl AsRef<Path>,
        manifest: impl AsRef<Path>,
    ) -> Result<(), MemoryRestoreError> {
        restore_offline_impl(
            live_database.as_ref(),
            backup_database.as_ref(),
            manifest.as_ref(),
            false,
        )
    }
}

pub(super) fn create_backup(
    store: &SqliteMemoryStore,
    data_dir: &Path,
) -> Result<MemoryBackupArtifact, MemoryStoreError> {
    let directory = data_dir.join("memory-backups");
    std::fs::create_dir_all(&directory).map_err(|_| backup_error())?;
    let created_at = crate::session::now_secs();
    let id = format!("{BACKUP_PREFIX}{created_at}-{}", uuid::Uuid::new_v4());
    let database_path = directory.join(format!("{id}{DATABASE_SUFFIX}"));
    let manifest_path = directory.join(format!("{id}{MANIFEST_SUFFIX}"));
    let database_temp = directory.join(format!(".{id}.sqlite3.tmp"));
    let manifest_temp = directory.join(format!(".{id}.manifest.tmp"));
    let result = (|| {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&database_temp)
            .map_err(|_| backup_error())?;
        secure_file(&database_temp)?;
        store.with_connection(|connection| {
            connection
                .backup(MAIN_DB, &database_temp, None)
                .map_err(|_| backup_error())
        })?;
        verify_database(&database_temp)?;
        sync_file(&database_temp)?;
        let (size_bytes, sha256) = digest_file(&database_temp)?;
        let manifest = MemoryBackupManifest {
            manifest_version: MANIFEST_VERSION,
            schema_version: SCHEMA_VERSION,
            created_at,
            size_bytes,
            sha256,
        };
        write_manifest(&manifest_temp, &manifest)?;
        std::fs::rename(&database_temp, &database_path).map_err(|_| backup_error())?;
        std::fs::rename(&manifest_temp, &manifest_path).map_err(|_| backup_error())?;
        sync_directory(&directory)?;
        let artifact = MemoryBackupArtifact {
            database_path,
            manifest_path,
            manifest,
        };
        verify_artifact_pair(&artifact)?;
        Ok(artifact)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(database_temp);
        let _ = std::fs::remove_file(manifest_temp);
    }
    result
}

pub(super) fn create_backup_and_rotate(
    store: &SqliteMemoryStore,
    data_dir: &Path,
    retained_copies: u32,
) -> Result<MemoryBackupArtifact, MemoryStoreError> {
    if !(MIN_RETAINED_COPIES..=MAX_RETAINED_COPIES).contains(&retained_copies) {
        return Err(backup_error());
    }
    let artifact = create_backup(store, data_dir)?;
    rotate_verified_pairs(data_dir, &artifact, retained_copies)?;
    Ok(artifact)
}

fn rotate_verified_pairs(
    data_dir: &Path,
    created: &MemoryBackupArtifact,
    retained_copies: u32,
) -> Result<(), MemoryStoreError> {
    let directory = data_dir.join("memory-backups");
    let mut pairs = verified_backup_pairs(&directory)?;
    pairs.sort_by(|left, right| {
        left.manifest
            .created_at
            .cmp(&right.manifest.created_at)
            .then_with(|| left.database_path.cmp(&right.database_path))
    });
    let excess = pairs
        .len()
        .saturating_sub(usize::try_from(retained_copies).map_err(|_| backup_error())?);
    let candidates = pairs
        .into_iter()
        .filter(|pair| pair.database_path != created.database_path)
        .take(excess);
    for pair in candidates {
        std::fs::remove_file(&pair.manifest_path).map_err(|_| backup_error())?;
        std::fs::remove_file(&pair.database_path).map_err(|_| backup_error())?;
        sync_directory(&directory)?;
    }
    Ok(())
}

fn verified_backup_pairs(directory: &Path) -> Result<Vec<MemoryBackupArtifact>, MemoryStoreError> {
    let mut pairs = Vec::new();
    for entry in std::fs::read_dir(directory).map_err(|_| backup_error())? {
        let entry = entry.map_err(|_| backup_error())?;
        if !entry.file_type().map_err(|_| backup_error())?.is_file() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(id) = name.strip_suffix(MANIFEST_SUFFIX) else {
            continue;
        };
        if !id.starts_with(BACKUP_PREFIX) {
            continue;
        }
        let database_path = directory.join(format!("{id}{DATABASE_SUFFIX}"));
        if !database_path.is_file() {
            continue;
        }
        let manifest_path = entry.path();
        let Ok(manifest) = read_manifest(&manifest_path) else {
            continue;
        };
        let artifact = MemoryBackupArtifact {
            database_path,
            manifest_path,
            manifest,
        };
        if verify_artifact_pair(&artifact).is_ok() {
            pairs.push(artifact);
        }
    }
    Ok(pairs)
}

fn verify_artifact_pair(artifact: &MemoryBackupArtifact) -> Result<(), MemoryStoreError> {
    validate_artifact(&artifact.database_path, &artifact.manifest)?;
    verify_database(&artifact.database_path)
}

fn restore_offline_impl(
    live: &Path,
    backup: &Path,
    manifest_path: &Path,
    fail_after_replace: bool,
) -> Result<(), MemoryRestoreError> {
    reject_live_sidecars(live).map_err(|_| MemoryRestoreError::Rejected)?;
    let manifest = read_manifest(manifest_path).map_err(|_| MemoryRestoreError::Rejected)?;
    validate_artifact(backup, &manifest).map_err(|_| MemoryRestoreError::Rejected)?;
    verify_database(backup).map_err(|_| MemoryRestoreError::Rejected)?;
    let parent = live
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or(MemoryRestoreError::Rejected)?;
    std::fs::create_dir_all(parent).map_err(|_| MemoryRestoreError::Rejected)?;
    let temp = parent.join(format!(".memory-restore-{}.tmp", uuid::Uuid::new_v4()));
    let rollback = parent.join(format!(".memory-rollback-{}", uuid::Uuid::new_v4()));
    if (|| {
        copy_new(backup, &temp)?;
        secure_file(&temp)?;
        validate_artifact(&temp, &manifest)?;
        verify_database(&temp)?;
        sync_file(&temp)
    })()
    .is_err()
    {
        let _ = std::fs::remove_file(temp);
        return Err(MemoryRestoreError::Rejected);
    }
    let had_live = live.exists();
    if had_live {
        if std::fs::rename(live, &rollback).is_err() {
            let _ = std::fs::remove_file(&temp);
            return Err(MemoryRestoreError::Rejected);
        }
        if sync_directory(parent).is_err() {
            let _ = std::fs::remove_file(&temp);
            return rollback_original(live, &rollback, parent);
        }
    }
    if std::fs::rename(&temp, live).is_err() {
        let _ = std::fs::remove_file(&temp);
        return if had_live {
            rollback_original(live, &rollback, parent)
        } else {
            Err(MemoryRestoreError::Rejected)
        };
    }
    if fail_after_replace || sync_directory(parent).is_err() {
        return if had_live {
            rollback_original(live, &rollback, parent)
        } else if std::fs::remove_file(live).is_ok() && sync_directory(parent).is_ok() {
            Err(MemoryRestoreError::Rejected)
        } else {
            Err(MemoryRestoreError::OutcomeUnknown)
        };
    }
    if had_live {
        let _ = std::fs::remove_file(rollback);
        let _ = sync_directory(parent);
    }
    Ok(())
}

fn rollback_original(
    live: &Path,
    rollback: &Path,
    parent: &Path,
) -> Result<(), MemoryRestoreError> {
    if std::fs::rename(rollback, live).is_ok() && sync_directory(parent).is_ok() {
        Err(MemoryRestoreError::Rejected)
    } else {
        Err(MemoryRestoreError::OutcomeUnknown)
    }
}

fn verify_database(path: &Path) -> Result<(), MemoryStoreError> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|_| backup_error())?;
    verify_schema(&connection).map_err(|_| backup_error())?;
    let version: i64 = connection
        .query_row(
            "SELECT version FROM memory_schema_migrations WHERE component = 'relationship_memory'",
            [],
            |row| row.get(0),
        )
        .map_err(|_| backup_error())?;
    let quick: String = connection
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .map_err(|_| backup_error())?;
    let foreign_key_failure: Option<i64> = connection
        .query_row(
            "SELECT 1 FROM pragma_foreign_key_check LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| backup_error())?;
    if version != SCHEMA_VERSION || quick != "ok" || foreign_key_failure.is_some() {
        return Err(backup_error());
    }
    Ok(())
}

fn validate_artifact(path: &Path, manifest: &MemoryBackupManifest) -> Result<(), MemoryStoreError> {
    if manifest.manifest_version != MANIFEST_VERSION
        || manifest.schema_version != SCHEMA_VERSION
        || manifest.created_at <= 0
    {
        return Err(backup_error());
    }
    let (size, digest) = digest_file(path)?;
    if size != manifest.size_bytes || digest != manifest.sha256 {
        return Err(backup_error());
    }
    Ok(())
}

fn read_manifest(path: &Path) -> Result<MemoryBackupManifest, MemoryStoreError> {
    if std::fs::metadata(path).map_err(|_| backup_error())?.len() > MAX_MANIFEST_BYTES {
        return Err(backup_error());
    }
    serde_json::from_slice(&std::fs::read(path).map_err(|_| backup_error())?)
        .map_err(|_| backup_error())
}

fn write_manifest(path: &Path, manifest: &MemoryBackupManifest) -> Result<(), MemoryStoreError> {
    let bytes = serde_json::to_vec(manifest).map_err(|_| backup_error())?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|_| backup_error())?;
    file.write_all(&bytes).map_err(|_| backup_error())?;
    file.sync_all().map_err(|_| backup_error())?;
    secure_file(path)
}

fn digest_file(path: &Path) -> Result<(u64, String), MemoryStoreError> {
    let mut file = File::open(path).map_err(|_| backup_error())?;
    let mut hasher = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|_| backup_error())?;
        if read == 0 {
            break;
        }
        size = size.checked_add(read as u64).ok_or_else(backup_error)?;
        hasher.update(&buffer[..read]);
    }
    let mut digest = String::with_capacity(64);
    for byte in hasher.finalize() {
        write!(&mut digest, "{byte:02x}").map_err(|_| backup_error())?;
    }
    Ok((size, digest))
}

fn sync_file(path: &Path) -> Result<(), MemoryStoreError> {
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|_| backup_error())
}

fn sync_directory(path: &Path) -> Result<(), MemoryStoreError> {
    sync_file(path)
}

fn copy_new(source: &Path, destination: &Path) -> Result<(), MemoryStoreError> {
    let mut source = File::open(source).map_err(|_| backup_error())?;
    let mut destination = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|_| backup_error())?;
    std::io::copy(&mut source, &mut destination).map_err(|_| backup_error())?;
    destination.sync_all().map_err(|_| backup_error())
}

fn reject_live_sidecars(path: &Path) -> Result<(), MemoryStoreError> {
    for suffix in ["-wal", "-shm"] {
        let mut sidecar = path.as_os_str().to_owned();
        sidecar.push(suffix);
        if PathBuf::from(sidecar).exists() {
            return Err(backup_error());
        }
    }
    Ok(())
}

#[cfg(unix)]
fn secure_file(path: &Path) -> Result<(), MemoryStoreError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|_| backup_error())
}

#[cfg(not(unix))]
fn secure_file(_: &Path) -> Result<(), MemoryStoreError> {
    Ok(())
}

fn backup_error() -> MemoryStoreError {
    MemoryStoreError::Store("memory backup operation failed".into())
}

#[cfg(test)]
#[path = "backup_tests.rs"]
mod tests;
