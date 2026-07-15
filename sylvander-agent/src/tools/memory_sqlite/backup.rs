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

/// Explicit offline administration surface. Restore must run before the live
/// database is opened by a Runtime or [`SqliteMemoryStore`].
#[derive(Debug, Clone, Copy, Default)]
pub struct SqliteMemoryAdmin;

impl SqliteMemoryAdmin {
    pub fn restore_offline(
        live_database: impl AsRef<Path>,
        backup_database: impl AsRef<Path>,
        manifest: impl AsRef<Path>,
    ) -> Result<(), MemoryStoreError> {
        restore_offline(
            live_database.as_ref(),
            backup_database.as_ref(),
            manifest.as_ref(),
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
    let id = format!("relationship-memory-{created_at}-{}", uuid::Uuid::new_v4());
    let database_path = directory.join(format!("{id}.sqlite3"));
    let manifest_path = directory.join(format!("{id}.manifest.json"));
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
        Ok(MemoryBackupArtifact {
            database_path,
            manifest_path,
            manifest,
        })
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(database_temp);
        let _ = std::fs::remove_file(manifest_temp);
    }
    result
}

fn restore_offline(
    live: &Path,
    backup: &Path,
    manifest_path: &Path,
) -> Result<(), MemoryStoreError> {
    reject_live_sidecars(live)?;
    let manifest = read_manifest(manifest_path)?;
    validate_artifact(backup, &manifest)?;
    verify_database(backup)?;
    let parent = live
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or_else(backup_error)?;
    std::fs::create_dir_all(parent).map_err(|_| backup_error())?;
    let temp = parent.join(format!(".memory-restore-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        copy_new(backup, &temp)?;
        secure_file(&temp)?;
        validate_artifact(&temp, &manifest)?;
        verify_database(&temp)?;
        sync_file(&temp)?;
        std::fs::rename(&temp, live).map_err(|_| backup_error())?;
        sync_directory(parent)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temp);
    }
    result
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
