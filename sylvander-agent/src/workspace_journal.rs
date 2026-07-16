//! Durable, session-scoped journal for reversible workspace mutations.

use std::collections::HashMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

const MAX_SNAPSHOT_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug)]
pub struct WorkspaceJournal {
    base: PathBuf,
    lock: Mutex<()>,
}

#[derive(Debug, Clone)]
pub struct PreparedMutation {
    manifest: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollbackReport {
    pub turn_id: String,
    pub restored: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollbackPreview {
    pub turn_id: String,
    pub files: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MutationState {
    Prepared,
    Applied,
    RolledBack,
    Abandoned,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Manifest {
    id: String,
    sequence: u64,
    session_id: String,
    turn_id: String,
    workspace: PathBuf,
    relative_path: String,
    before_blob: Option<PathBuf>,
    after_blob: PathBuf,
    state: MutationState,
}

#[derive(Debug, Serialize, Deserialize)]
struct RecoveryMarker {
    manifests: Vec<PathBuf>,
}

impl WorkspaceJournal {
    #[must_use]
    pub fn new(base: impl Into<PathBuf>) -> Self {
        Self {
            base: base.into(),
            lock: Mutex::new(()),
        }
    }

    pub fn resolve(root: &Path, relative: &str) -> Result<PathBuf, String> {
        let relative = Path::new(relative);
        if relative.is_absolute() || relative.as_os_str().is_empty() {
            return Err("workspace path must be a non-empty relative path".into());
        }
        if relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        }) {
            return Err("workspace path cannot escape through `..`".into());
        }
        let target = root.join(relative);
        let mut cursor = root.to_path_buf();
        for component in relative.components() {
            cursor.push(component);
            if cursor
                .symlink_metadata()
                .is_ok_and(|metadata| metadata.file_type().is_symlink())
            {
                return Err(format!(
                    "workspace path crosses symbolic link `{}`",
                    cursor.display()
                ));
            }
        }
        Ok(target)
    }

    pub fn prepare(
        &self,
        session_id: &str,
        turn_id: &str,
        workspace: &Path,
        relative_path: &str,
        after: &[u8],
    ) -> Result<PreparedMutation, String> {
        if after.len() as u64 > MAX_SNAPSHOT_BYTES {
            return Err("file exceeds the 8 MiB rollback snapshot limit".into());
        }
        let _guard = self
            .lock
            .lock()
            .map_err(|_| "workspace journal lock poisoned")?;
        self.recover_locked(session_id)?;
        let target = Self::resolve(workspace, relative_path)?;
        let before = match fs::symlink_metadata(&target) {
            Ok(metadata) if metadata.is_file() => {
                if metadata.len() > MAX_SNAPSHOT_BYTES {
                    return Err("existing file exceeds the 8 MiB rollback snapshot limit".into());
                }
                Some(fs::read(&target).map_err(|error| error.to_string())?)
            }
            Ok(_) => return Err("rollback journal only supports regular files".into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.to_string()),
        };
        let session_dir = self.session_dir(session_id);
        let entries = session_dir.join("entries");
        let blobs = session_dir.join("blobs");
        fs::create_dir_all(&entries).map_err(|error| error.to_string())?;
        fs::create_dir_all(&blobs).map_err(|error| error.to_string())?;
        let id = uuid::Uuid::new_v4().to_string();
        let before_blob = before
            .as_ref()
            .map(|bytes| {
                let path = blobs.join(format!("{id}.before"));
                write_file_atomic(&path, bytes).map(|()| path)
            })
            .transpose()?;
        let after_blob = blobs.join(format!("{id}.after"));
        write_file_atomic(&after_blob, after)?;
        let manifest_path = entries.join(format!("{id}.json"));
        let manifest = Manifest {
            id,
            sequence: next_sequence(&entries)?,
            session_id: session_id.into(),
            turn_id: turn_id.into(),
            workspace: workspace.to_path_buf(),
            relative_path: relative_path.into(),
            before_blob,
            after_blob,
            state: MutationState::Prepared,
        };
        write_json_atomic(&manifest_path, &manifest)?;
        Ok(PreparedMutation {
            manifest: manifest_path,
        })
    }

    pub fn commit(&self, prepared: &PreparedMutation) -> Result<(), String> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| "workspace journal lock poisoned")?;
        let mut manifest = read_manifest(&prepared.manifest)?;
        if manifest.state != MutationState::Prepared {
            return Err("workspace mutation is not awaiting commit".into());
        }
        let target = Self::resolve(&manifest.workspace, &manifest.relative_path)?;
        let current = fs::read(&target).map_err(|error| error.to_string())?;
        let expected = fs::read(&manifest.after_blob).map_err(|error| error.to_string())?;
        if current != expected {
            return Err("workspace changed before journal commit".into());
        }
        manifest.state = MutationState::Applied;
        write_json_atomic(&prepared.manifest, &manifest)
    }

    pub fn preview_latest_turn(&self, session_id: &str) -> Result<RollbackPreview, String> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| "workspace journal lock poisoned")?;
        self.recover_locked(session_id)?;
        let mut manifests = self.active_manifests(session_id)?;
        let turn_id = manifests
            .last()
            .map(|(_, manifest)| manifest.turn_id.clone())
            .ok_or_else(|| "no reversible workspace changes for this session".to_string())?;
        manifests.retain(|(_, manifest)| manifest.turn_id == turn_id);
        preflight_reverse(&manifests, false)?;
        let mut files = manifests
            .iter()
            .map(|(_, manifest)| manifest.relative_path.clone())
            .collect::<Vec<_>>();
        files.sort();
        files.dedup();
        Ok(RollbackPreview { turn_id, files })
    }

    pub fn rollback_latest_turn(
        &self,
        session_id: &str,
        expected_turn_id: &str,
    ) -> Result<RollbackReport, String> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| "workspace journal lock poisoned")?;
        self.recover_locked(session_id)?;
        let mut manifests = self.active_manifests(session_id)?;
        let turn_id = manifests
            .last()
            .map(|(_, manifest)| manifest.turn_id.clone())
            .ok_or_else(|| "no reversible workspace changes for this session".to_string())?;
        if turn_id != expected_turn_id {
            return Err("workspace changed since rollback confirmation; inspect again".into());
        }
        manifests.retain(|(_, manifest)| manifest.turn_id == turn_id);
        preflight_reverse(&manifests, false)?;
        let marker_path = self.session_dir(session_id).join("rollback.json");
        write_json_atomic(
            &marker_path,
            &RecoveryMarker {
                manifests: manifests.iter().map(|(path, _)| path.clone()).collect(),
            },
        )?;
        for (_, manifest) in manifests.iter().rev() {
            restore_before(manifest)?;
        }
        let restored = manifests
            .iter()
            .map(|(_, manifest)| manifest.relative_path.clone())
            .collect();
        for (path, manifest) in &mut manifests {
            manifest.state = MutationState::RolledBack;
            write_json_atomic(path, manifest)?;
        }
        fs::remove_file(marker_path).map_err(|error| error.to_string())?;
        Ok(RollbackReport { turn_id, restored })
    }

    fn recover_locked(&self, session_id: &str) -> Result<(), String> {
        let marker_path = self.session_dir(session_id).join("rollback.json");
        let Ok(bytes) = fs::read(&marker_path) else {
            return Ok(());
        };
        let marker: RecoveryMarker = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        let mut manifests = marker
            .manifests
            .iter()
            .map(|path| read_manifest(path).map(|manifest| (path.clone(), manifest)))
            .collect::<Result<Vec<_>, _>>()?;
        preflight_reverse(&manifests, true)?;
        for (_, manifest) in manifests.iter().rev() {
            restore_before(manifest)?;
        }
        for (path, manifest) in &mut manifests {
            manifest.state = MutationState::RolledBack;
            write_json_atomic(path, manifest)?;
        }
        fs::remove_file(marker_path).map_err(|error| error.to_string())
    }

    fn active_manifests(&self, session_id: &str) -> Result<Vec<(PathBuf, Manifest)>, String> {
        let entries = self.session_dir(session_id).join("entries");
        let mut result = Vec::new();
        let Ok(iter) = fs::read_dir(entries) else {
            return Ok(result);
        };
        for entry in iter {
            let path = entry.map_err(|error| error.to_string())?.path();
            let mut manifest = read_manifest(&path)?;
            if manifest.state == MutationState::Prepared {
                let target = Self::resolve(&manifest.workspace, &manifest.relative_path)?;
                let current = fs::read(&target).ok();
                let after = fs::read(&manifest.after_blob).map_err(|error| error.to_string())?;
                manifest.state = if current.as_deref() == Some(after.as_slice()) {
                    MutationState::Applied
                } else {
                    MutationState::Abandoned
                };
                write_json_atomic(&path, &manifest)?;
            }
            if manifest.state == MutationState::Applied {
                result.push((path, manifest));
            }
        }
        result.sort_by_key(|(_, manifest)| manifest.sequence);
        Ok(result)
    }

    fn session_dir(&self, session_id: &str) -> PathBuf {
        self.base.join(session_id.replace(['/', '\\'], "_"))
    }
}

fn restore_before(manifest: &Manifest) -> Result<(), String> {
    let target = WorkspaceJournal::resolve(&manifest.workspace, &manifest.relative_path)?;
    match &manifest.before_blob {
        Some(blob) => {
            let bytes = fs::read(blob).map_err(|error| error.to_string())?;
            write_file_atomic(&target, &bytes)
        }
        None => match fs::remove_file(target) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.to_string()),
        },
    }
}

fn preflight_reverse(
    manifests: &[(PathBuf, Manifest)],
    allow_already_restored: bool,
) -> Result<(), String> {
    let mut virtual_files: HashMap<PathBuf, Option<Vec<u8>>> = HashMap::new();
    for (_, manifest) in manifests.iter().rev() {
        let target = WorkspaceJournal::resolve(&manifest.workspace, &manifest.relative_path)?;
        let current = virtual_files
            .entry(target.clone())
            .or_insert_with(|| fs::read(&target).ok());
        let after = fs::read(&manifest.after_blob).map_err(|error| error.to_string())?;
        let before = manifest
            .before_blob
            .as_ref()
            .map(|path| fs::read(path).map_err(|error| error.to_string()))
            .transpose()?;
        if current.as_deref() == Some(after.as_slice()) {
            *current = before;
        } else if allow_already_restored && *current == before {
            // Recovery after a crash may observe this mutation already reversed.
        } else {
            return Err(format!(
                "rollback conflict: `{}` changed after the agent edit",
                manifest.relative_path
            ));
        }
    }
    Ok(())
}

fn next_sequence(entries: &Path) -> Result<u64, String> {
    let mut max = 0;
    for entry in fs::read_dir(entries).map_err(|error| error.to_string())? {
        let manifest = read_manifest(&entry.map_err(|error| error.to_string())?.path())?;
        max = max.max(manifest.sequence);
    }
    Ok(max + 1)
}

fn read_manifest(path: &Path) -> Result<Manifest, String> {
    serde_json::from_slice(&fs::read(path).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())
}

fn write_json_atomic(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    write_file_atomic(path, &bytes)
}

fn write_file_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "atomic write target has no parent".to_string())?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let temp = parent.join(format!(".sylvander-{}.tmp", uuid::Uuid::new_v4()));
    fs::write(&temp, bytes).map_err(|error| error.to_string())?;
    fs::OpenOptions::new()
        .read(true)
        .open(&temp)
        .and_then(|file| file.sync_all())
        .map_err(|error| error.to_string())?;
    fs::rename(&temp, path).map_err(|error| error.to_string())?;
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| error.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn multiple_edits_in_one_turn_roll_back_in_reverse_order() {
        let root = TempDir::new().unwrap();
        let data = TempDir::new().unwrap();
        let file = root.path().join("src.txt");
        fs::write(&file, "zero").unwrap();
        let journal = WorkspaceJournal::new(data.path());

        let first = journal
            .prepare("s", "turn-1", root.path(), "src.txt", b"one")
            .unwrap();
        fs::write(&file, "one").unwrap();
        journal.commit(&first).unwrap();
        let second = journal
            .prepare("s", "turn-1", root.path(), "src.txt", b"two")
            .unwrap();
        fs::write(&file, "two").unwrap();
        journal.commit(&second).unwrap();

        let preview = journal.preview_latest_turn("s").unwrap();
        let report = journal.rollback_latest_turn("s", &preview.turn_id).unwrap();
        assert_eq!(report.turn_id, "turn-1");
        assert_eq!(fs::read_to_string(file).unwrap(), "zero");
    }

    #[test]
    fn conflict_refuses_to_overwrite_external_changes() {
        let root = TempDir::new().unwrap();
        let data = TempDir::new().unwrap();
        let file = root.path().join("src.txt");
        fs::write(&file, "before").unwrap();
        let journal = WorkspaceJournal::new(data.path());
        let mutation = journal
            .prepare("s", "turn-1", root.path(), "src.txt", b"agent")
            .unwrap();
        fs::write(&file, "agent").unwrap();
        journal.commit(&mutation).unwrap();
        fs::write(&file, "external").unwrap();

        assert!(
            journal
                .rollback_latest_turn("s", "turn-1")
                .unwrap_err()
                .contains("conflict")
        );
        assert_eq!(fs::read_to_string(file).unwrap(), "external");
    }

    #[test]
    fn prepared_crash_record_and_new_file_are_recoverable() {
        let root = TempDir::new().unwrap();
        let data = TempDir::new().unwrap();
        let file = root.path().join("new.txt");
        let journal = WorkspaceJournal::new(data.path());
        let _pending = journal
            .prepare("s", "turn-1", root.path(), "new.txt", b"created")
            .unwrap();
        fs::write(&file, "created").unwrap();

        journal.rollback_latest_turn("s", "turn-1").unwrap();
        assert!(!file.exists());
    }

    #[cfg(unix)]
    #[test]
    fn resolver_rejects_parent_escape_and_symlink_hops() {
        use std::os::unix::fs::symlink;
        let root = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        symlink(outside.path(), root.path().join("link")).unwrap();
        assert!(WorkspaceJournal::resolve(root.path(), "../outside").is_err());
        assert!(WorkspaceJournal::resolve(root.path(), "link/file").is_err());
    }
}
