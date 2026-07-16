//! Git worktree lifecycle used to isolate coding sessions.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct GitWorktreeManager {
    base: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceLease {
    pub session_id: String,
    pub source_root: PathBuf,
    pub worktree_root: PathBuf,
    pub effective_workspace: PathBuf,
    pub branch: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceDiff {
    pub status: String,
    pub patch: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewedMerge {
    pub previous_commit: String,
    pub candidate_commit: String,
    pub merge_commit: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorktreeReconciliation {
    pub retained: usize,
    pub removed: usize,
}

impl GitWorktreeManager {
    #[must_use]
    pub fn new(base: impl Into<PathBuf>) -> Self {
        Self { base: base.into() }
    }

    #[must_use]
    pub fn is_git_workspace(&self, requested: &Path) -> bool {
        requested.is_dir()
            && git_output(requested, &["rev-parse", "--show-toplevel"])
                .is_ok_and(|output| output.status.success())
    }

    /// Create a session branch and return the isolated equivalent of the
    /// requested workspace (including a workspace that is below the repo root).
    pub fn create(&self, session_id: &str, requested: &Path) -> Result<WorkspaceLease, String> {
        if session_id.is_empty()
            || !session_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err("session id is not safe for a worktree branch".into());
        }
        let requested = requested.canonicalize().map_err(display_error)?;
        let source_root = git_text(&requested, &["rev-parse", "--show-toplevel"])?;
        let source_root = PathBuf::from(source_root);
        if !git_text(&source_root, &["status", "--porcelain"])?.is_empty() {
            return Err(
                "source workspace has uncommitted changes; commit or stash them before starting an isolated coding session"
                    .into(),
            );
        }
        let relative = requested
            .strip_prefix(&source_root)
            .map_err(|_| "workspace is outside its git repository")?;
        let branch = format!("sylvander/{session_id}");
        let worktree_root = self.base.join("worktrees").join(session_id);
        if worktree_root.exists() || self.manifest_path(session_id).exists() {
            return Err("session worktree already exists".into());
        }
        fs::create_dir_all(worktree_root.parent().expect("worktree has parent"))
            .map_err(display_error)?;
        git_ok(
            &source_root,
            &[
                "worktree",
                "add",
                "-b",
                &branch,
                path_text(&worktree_root)?,
                "HEAD",
            ],
        )?;
        let lease = WorkspaceLease {
            session_id: session_id.into(),
            source_root,
            effective_workspace: worktree_root.join(relative),
            worktree_root,
            branch,
        };
        if let Err(error) = self.save(&lease) {
            let _ = git_ok(
                &lease.source_root,
                &[
                    "worktree",
                    "remove",
                    "--force",
                    path_text(&lease.worktree_root)?,
                ],
            );
            let _ = git_ok(&lease.source_root, &["branch", "-D", &lease.branch]);
            return Err(error);
        }
        Ok(lease)
    }

    pub fn open(&self, session_id: &str) -> Result<WorkspaceLease, String> {
        let bytes = fs::read(self.manifest_path(session_id)).map_err(display_error)?;
        serde_json::from_slice(&bytes).map_err(display_error)
    }

    /// Reconcile durable lease manifests with the sessions restored by Runtime.
    ///
    /// Active leases are validated against their durable effective workspace.
    /// Leases belonging to deleted sessions and worktree directories left
    /// behind before a manifest was committed are removed.
    pub fn reconcile(
        &self,
        active: &HashMap<String, PathBuf>,
    ) -> Result<WorktreeReconciliation, String> {
        let mut remaining = active.clone();
        let mut retained_roots = HashSet::new();
        let mut retained = 0;
        let mut removed = 0;
        let leases = self.base.join("leases");
        if leases.is_dir() {
            let mut manifests = fs::read_dir(&leases)
                .map_err(display_error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(display_error)?;
            manifests.sort_by_key(fs::DirEntry::file_name);
            for entry in manifests {
                if !entry.file_type().map_err(display_error)?.is_file()
                    || entry.path().extension().and_then(|value| value.to_str()) != Some("json")
                {
                    continue;
                }
                let lease: WorkspaceLease =
                    serde_json::from_slice(&fs::read(entry.path()).map_err(display_error)?)
                        .map_err(display_error)?;
                let expected_name = format!("{}.json", lease.session_id);
                if entry.file_name() != expected_name.as_str() {
                    return Err("worktree lease filename does not match its session".into());
                }
                self.validate_lease(&lease)?;
                if let Some(expected_workspace) = remaining.remove(&lease.session_id) {
                    if canonical(&lease.effective_workspace)? != canonical(&expected_workspace)? {
                        return Err(format!(
                            "worktree lease workspace does not match session {}",
                            lease.session_id
                        ));
                    }
                    retained_roots.insert(canonical(&lease.worktree_root)?);
                    retained += 1;
                } else {
                    self.cleanup(&lease)?;
                    removed += 1;
                }
            }
        }
        if let Some(session_id) = remaining.keys().min() {
            return Err(format!(
                "session {session_id} references a missing worktree lease"
            ));
        }

        let worktrees = self.base.join("worktrees");
        if worktrees.is_dir() {
            let mut directories = fs::read_dir(&worktrees)
                .map_err(display_error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(display_error)?;
            directories.sort_by_key(fs::DirEntry::file_name);
            for entry in directories {
                if !entry.file_type().map_err(display_error)?.is_dir() {
                    continue;
                }
                let path = canonical(&entry.path())?;
                if retained_roots.contains(&path) {
                    continue;
                }
                Self::cleanup_unmanifested(&path)?;
                removed += 1;
            }
        }
        Ok(WorktreeReconciliation { retained, removed })
    }

    pub fn inspect(&self, lease: &WorkspaceLease) -> Result<WorkspaceDiff, String> {
        let mut patch = git_text(&lease.worktree_root, &["diff", "--binary", "HEAD"])?;
        let untracked = git_text(
            &lease.worktree_root,
            &["ls-files", "--others", "--exclude-standard"],
        )?;
        for relative in untracked.lines().filter(|line| !line.is_empty()) {
            let output = git_output(
                &lease.worktree_root,
                &[
                    "diff",
                    "--no-index",
                    "--binary",
                    "--",
                    null_device(),
                    relative,
                ],
            )?;
            if !matches!(output.status.code(), Some(0 | 1)) {
                return Err(git_failure(&output));
            }
            if !patch.is_empty() {
                patch.push('\n');
            }
            patch.push_str(&String::from_utf8(output.stdout).map_err(display_error)?);
        }
        Ok(WorkspaceDiff {
            status: git_text(&lease.worktree_root, &["status", "--short"])?,
            patch,
        })
    }

    /// Commit the reviewed worktree contents and merge them into the source
    /// checkout. The lease remains active so the coding session can continue.
    pub fn accept(&self, lease: &WorkspaceLease) -> Result<(), String> {
        self.accept_reviewed(lease).map(|_| ())
    }

    /// Merge reviewed work with an explicit merge commit so a later observed
    /// regression can be reverted without rewriting source history.
    pub fn accept_reviewed(&self, lease: &WorkspaceLease) -> Result<Option<ReviewedMerge>, String> {
        git_ok(&lease.worktree_root, &["add", "-A"])?;
        let staged = git_output(&lease.worktree_root, &["diff", "--cached", "--quiet"])?;
        if staged.status.success() {
            return Ok(None);
        }
        let previous_commit = git_text(&lease.source_root, &["rev-parse", "HEAD"])?;
        git_ok(
            &lease.worktree_root,
            &[
                "-c",
                "user.name=Sylvander",
                "-c",
                "user.email=sylvander@localhost",
                "commit",
                "-m",
                &format!("chore: accept session {}", lease.session_id),
            ],
        )?;
        let candidate_commit = git_text(&lease.worktree_root, &["rev-parse", "HEAD"])?;
        git_ok(
            &lease.source_root,
            &["merge", "--no-ff", "--no-edit", &lease.branch],
        )?;
        let merge_commit = git_text(&lease.source_root, &["rev-parse", "HEAD"])?;
        Ok(Some(ReviewedMerge {
            previous_commit,
            candidate_commit,
            merge_commit,
        }))
    }

    /// Revert only the still-current reviewed merge. If source has advanced,
    /// stop for a human instead of reverting unrelated later work.
    pub fn rollback_reviewed(
        &self,
        lease: &WorkspaceLease,
        merge_commit: &str,
    ) -> Result<String, String> {
        if merge_commit.len() != 40
            || !merge_commit.bytes().all(|byte| byte.is_ascii_hexdigit())
            || git_text(&lease.source_root, &["rev-parse", "HEAD"])? != merge_commit
        {
            return Err("reviewed merge is not the current source commit".into());
        }
        git_ok(
            &lease.source_root,
            &[
                "-c",
                "user.name=Sylvander",
                "-c",
                "user.email=sylvander@localhost",
                "revert",
                "-m",
                "1",
                "--no-edit",
                merge_commit,
            ],
        )?;
        git_text(&lease.source_root, &["rev-parse", "HEAD"])
    }

    pub fn discard(&self, lease: &WorkspaceLease) -> Result<(), String> {
        self.cleanup(lease)
    }

    fn cleanup(&self, lease: &WorkspaceLease) -> Result<(), String> {
        remove_worktree_and_branch(&lease.source_root, &lease.worktree_root, &lease.branch)?;
        let manifest = self.manifest_path(&lease.session_id);
        if manifest.exists() {
            fs::remove_file(manifest).map_err(display_error)?;
        }
        Ok(())
    }

    fn save(&self, lease: &WorkspaceLease) -> Result<(), String> {
        let path = self.manifest_path(&lease.session_id);
        fs::create_dir_all(path.parent().expect("manifest has parent")).map_err(display_error)?;
        fs::write(path, serde_json::to_vec(lease).map_err(display_error)?).map_err(display_error)
    }

    fn manifest_path(&self, session_id: &str) -> PathBuf {
        self.base.join("leases").join(format!("{session_id}.json"))
    }

    fn validate_lease(&self, lease: &WorkspaceLease) -> Result<(), String> {
        if lease.session_id.is_empty()
            || !lease
                .session_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            || lease.branch != format!("sylvander/{}", lease.session_id)
        {
            return Err("invalid worktree lease identity".into());
        }
        let source = canonical(&lease.source_root)?;
        let worktree = canonical(&lease.worktree_root)?;
        let effective = canonical(&lease.effective_workspace)?;
        let managed = canonical(&self.base.join("worktrees"))?;
        if !worktree.starts_with(&managed) || !effective.starts_with(&worktree) {
            return Err("worktree lease escapes its managed directory".into());
        }
        let actual_source = PathBuf::from(git_text(&source, &["rev-parse", "--show-toplevel"])?);
        let actual_worktree =
            PathBuf::from(git_text(&worktree, &["rev-parse", "--show-toplevel"])?);
        if canonical(&actual_source)? != source || canonical(&actual_worktree)? != worktree {
            return Err("worktree lease does not match its Git repository".into());
        }
        Ok(())
    }

    fn cleanup_unmanifested(worktree: &Path) -> Result<(), String> {
        let common = PathBuf::from(git_text(
            worktree,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )?);
        let source_root = common
            .parent()
            .ok_or_else(|| "orphan worktree has no source repository".to_string())?;
        let branch = git_text(worktree, &["branch", "--show-current"])?;
        if !branch.starts_with("sylvander/") {
            return Err("refusing to remove an unmanaged worktree branch".into());
        }
        remove_worktree_and_branch(source_root, worktree, &branch)
    }
}

fn canonical(path: &Path) -> Result<PathBuf, String> {
    path.canonicalize().map_err(display_error)
}

fn remove_worktree_and_branch(
    source_root: &Path,
    worktree_root: &Path,
    branch: &str,
) -> Result<(), String> {
    let removal = git_output(
        source_root,
        &["worktree", "remove", "--force", path_text(worktree_root)?],
    )?;
    if !removal.status.success() && worktree_root.exists() {
        return Err(git_failure(&removal));
    }
    let deletion = git_output(source_root, &["branch", "-D", branch])?;
    if !deletion.status.success() {
        let exists = git_output(
            source_root,
            &[
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ],
        )?;
        if exists.status.success() {
            return Err(git_failure(&deletion));
        }
    }
    Ok(())
}

fn git_text(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let output = git_output(cwd, args)?;
    if !output.status.success() {
        return Err(git_failure(&output));
    }
    String::from_utf8(output.stdout)
        .map(|text| text.trim_end().to_string())
        .map_err(display_error)
}

fn git_ok(cwd: &Path, args: &[&str]) -> Result<(), String> {
    let output = git_output(cwd, args)?;
    output
        .status
        .success()
        .then_some(())
        .ok_or_else(|| git_failure(&output))
}

fn git_output(cwd: &Path, args: &[&str]) -> Result<Output, String> {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(display_error)
}

fn git_failure(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).trim().to_string()
}

fn path_text(path: &Path) -> Result<&str, String> {
    path.to_str()
        .ok_or_else(|| "worktree path is not valid UTF-8".into())
}

#[cfg(unix)]
const fn null_device() -> &'static str {
    "/dev/null"
}

#[cfg(windows)]
const fn null_device() -> &'static str {
    "NUL"
}

fn display_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> tempfile::TempDir {
        let repo = tempfile::tempdir().unwrap();
        git_ok(repo.path(), &["init", "-b", "master"]).unwrap();
        fs::write(repo.path().join("tracked.txt"), "before\n").unwrap();
        git_ok(repo.path(), &["add", "."]).unwrap();
        git_ok(
            repo.path(),
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "initial",
            ],
        )
        .unwrap();
        repo
    }

    #[test]
    fn create_edit_inspect_and_accept_merge() {
        let repo = repo();
        let state = tempfile::tempdir().unwrap();
        let manager = GitWorktreeManager::new(state.path());
        let lease = manager.create("session-1", repo.path()).unwrap();
        fs::write(lease.effective_workspace.join("tracked.txt"), "after\n").unwrap();
        fs::write(lease.effective_workspace.join("new.txt"), "new\n").unwrap();
        let diff = manager.inspect(&lease).unwrap();
        assert!(diff.status.contains("M tracked.txt"));
        assert!(diff.status.contains("?? new.txt"));
        assert!(diff.patch.contains("+after"));
        assert!(diff.patch.contains("+new"));
        manager.accept(&lease).unwrap();
        assert_eq!(
            fs::read_to_string(repo.path().join("tracked.txt")).unwrap(),
            "after\n"
        );
        assert!(lease.worktree_root.exists());
        manager.discard(&lease).unwrap();
    }

    #[test]
    fn discard_removes_changes_without_touching_source() {
        let repo = repo();
        let state = tempfile::tempdir().unwrap();
        let manager = GitWorktreeManager::new(state.path());
        let lease = manager.create("session-2", repo.path()).unwrap();
        fs::write(lease.effective_workspace.join("tracked.txt"), "discarded\n").unwrap();
        manager.discard(&lease).unwrap();
        assert_eq!(
            fs::read_to_string(repo.path().join("tracked.txt")).unwrap(),
            "before\n"
        );
        assert!(manager.open("session-2").is_err());
    }

    #[test]
    fn reviewed_merge_can_be_reverted_only_before_source_advances() {
        let repo = repo();
        let state = tempfile::tempdir().unwrap();
        let manager = GitWorktreeManager::new(state.path());
        let lease = manager.create("experiment-1", repo.path()).unwrap();
        fs::write(lease.effective_workspace.join("tracked.txt"), "candidate\n").unwrap();
        let reviewed = manager.accept_reviewed(&lease).unwrap().unwrap();
        assert_ne!(reviewed.previous_commit, reviewed.merge_commit);
        assert_ne!(reviewed.candidate_commit, reviewed.merge_commit);
        assert_eq!(
            fs::read_to_string(repo.path().join("tracked.txt")).unwrap(),
            "candidate\n"
        );
        let rollback_commit = manager
            .rollback_reviewed(&lease, &reviewed.merge_commit)
            .unwrap();
        assert_ne!(rollback_commit, reviewed.merge_commit);
        assert_eq!(
            fs::read_to_string(repo.path().join("tracked.txt")).unwrap(),
            "before\n"
        );
        assert!(
            manager
                .rollback_reviewed(&lease, &reviewed.merge_commit)
                .is_err()
        );
        manager.discard(&lease).unwrap();
    }

    #[test]
    fn reconcile_retains_active_and_removes_deleted_session_leases() {
        let repo = repo();
        let state = tempfile::tempdir().unwrap();
        let manager = GitWorktreeManager::new(state.path());
        let active = manager.create("active", repo.path()).unwrap();
        let deleted = manager.create("stale", repo.path()).unwrap();
        let report = manager
            .reconcile(&HashMap::from([(
                active.session_id.clone(),
                active.effective_workspace.clone(),
            )]))
            .unwrap();

        assert_eq!(
            report,
            WorktreeReconciliation {
                retained: 1,
                removed: 1
            }
        );
        assert!(active.worktree_root.is_dir());
        assert!(!deleted.worktree_root.exists());
        assert!(manager.open("active").is_ok());
        assert!(manager.open("stale").is_err());
        manager.discard(&active).unwrap();
    }

    #[test]
    fn reconcile_removes_worktree_left_before_manifest_commit() {
        let repo = repo();
        let state = tempfile::tempdir().unwrap();
        let manager = GitWorktreeManager::new(state.path());
        let orphan = manager.create("orphan", repo.path()).unwrap();
        fs::remove_file(manager.manifest_path("orphan")).unwrap();

        let report = manager.reconcile(&HashMap::new()).unwrap();

        assert_eq!(report.retained, 0);
        assert_eq!(report.removed, 1);
        assert!(!orphan.worktree_root.exists());
        let branch = git_output(
            repo.path(),
            &[
                "show-ref",
                "--verify",
                "--quiet",
                "refs/heads/sylvander/orphan",
            ],
        )
        .unwrap();
        assert!(!branch.status.success());
    }

    #[test]
    fn reconcile_rejects_missing_or_mismatched_active_lease() {
        let repo = repo();
        let state = tempfile::tempdir().unwrap();
        let manager = GitWorktreeManager::new(state.path());
        let lease = manager.create("active", repo.path()).unwrap();

        let mismatch = manager
            .reconcile(&HashMap::from([(
                lease.session_id.clone(),
                repo.path().to_path_buf(),
            )]))
            .unwrap_err();
        assert!(mismatch.contains("does not match"));

        manager.discard(&lease).unwrap();
        let missing = manager
            .reconcile(&HashMap::from([(
                "missing".into(),
                repo.path().to_path_buf(),
            )]))
            .unwrap_err();
        assert!(missing.contains("missing worktree lease"));
    }
}
