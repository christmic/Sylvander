//! Durable Git worktree leases whose repository lives on a remote executor.
//!
//! The server persists an intent before asking the remote host to create a
//! worktree. Startup reconciliation can therefore remove a worktree left by a
//! crash between remote creation and local manifest activation.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sylvander_agent::workspace_executor::WorkspaceExecutor;
use tokio::sync::Mutex;

use crate::git_worktree::{WorkspaceDiff, WorktreeReconciliation};

mod manifest;
mod transport;

pub use manifest::RemoteWorkspaceLease;
use manifest::{
    LeaseManifestStore, RemoteLeaseState, validate_relative, validate_remote_absolute,
    validate_session_id,
};
use transport::{RemoteGitTransport, shell_quote};

const MAX_REVIEW_BYTES: usize = 1024 * 1024;
const MAX_UNTRACKED_FILES: usize = 512;

/// Executor-backed manager for one configured SSH target.
#[derive(Clone)]
pub struct RemoteGitWorktreeManager {
    manifests: LeaseManifestStore,
    remote_root: PathBuf,
    target_id: String,
    git: RemoteGitTransport,
    mutation_lock: Arc<Mutex<()>>,
}

impl RemoteGitWorktreeManager {
    /// Construct one manager. Remote paths are POSIX paths interpreted only by
    /// the configured executor; local manifests live below `state_root`.
    pub fn new(
        state_root: impl Into<PathBuf>,
        remote_root: impl Into<PathBuf>,
        target_id: impl Into<String>,
        executor: Arc<dyn WorkspaceExecutor>,
    ) -> Result<Self, String> {
        let remote_root = remote_root.into();
        validate_remote_absolute(&remote_root)?;
        if remote_root == Path::new("/") {
            return Err("remote worktree root must not be the filesystem root".into());
        }
        let target_id = target_id.into();
        if target_id.trim().is_empty() || target_id.chars().any(char::is_control) {
            return Err("remote worktree target id is invalid".into());
        }
        Ok(Self {
            manifests: LeaseManifestStore::new(
                state_root.into(),
                remote_root.clone(),
                target_id.clone(),
            ),
            remote_root,
            git: RemoteGitTransport::new(target_id.clone(), executor),
            target_id,
            mutation_lock: Arc::new(Mutex::new(())),
        })
    }

    /// Return whether the requested remote directory is a Git worktree.
    pub async fn is_git_workspace(&self, requested: &Path) -> bool {
        validate_remote_absolute(requested).is_ok()
            && self
                .git
                .text(requested, &["rev-parse", "--show-toplevel"])
                .await
                .is_ok()
    }

    /// Create a durable, isolated worktree for one session.
    pub async fn create(
        &self,
        session_id: &str,
        requested: &Path,
    ) -> Result<RemoteWorkspaceLease, String> {
        let _guard = self.mutation_lock.lock().await;
        validate_session_id(session_id)?;
        validate_remote_absolute(requested)?;
        if self.manifests.path(session_id).exists() {
            return Err("session worktree already exists".into());
        }

        let source_root = PathBuf::from(
            self.git
                .text(requested, &["rev-parse", "--show-toplevel"])
                .await?,
        );
        validate_remote_absolute(&source_root)?;
        let source_prefix = self
            .git
            .text(requested, &["rev-parse", "--show-prefix"])
            .await?;
        let source_prefix = source_prefix.trim_end_matches('/');
        if !source_prefix.is_empty() {
            validate_relative(source_prefix)?;
        }
        if !self
            .git
            .text(&source_root, &["status", "--porcelain"])
            .await?
            .is_empty()
        {
            return Err(
                "source workspace has uncommitted changes; commit or stash them before starting an isolated coding session"
                    .into(),
            );
        }
        let worktree_root = self.remote_root.join(session_id);
        let mut lease = RemoteWorkspaceLease {
            session_id: session_id.into(),
            target_id: self.target_id.clone(),
            source_root,
            effective_workspace: worktree_root.join(source_prefix),
            worktree_root,
            branch: format!("sylvander/{session_id}"),
            state: RemoteLeaseState::Creating,
        };
        self.manifests.save(&lease)?;

        let creation = async {
            self.git
                .command_ok(
                    &lease.source_root,
                    &format!("mkdir -p -- {}", shell_quote(path_text(&self.remote_root)?)),
                    true,
                )
                .await?;
            self.git
                .ok(
                    &lease.source_root,
                    &[
                        "worktree",
                        "add",
                        "-b",
                        &lease.branch,
                        path_text(&lease.worktree_root)?,
                        "HEAD",
                    ],
                    true,
                )
                .await
        }
        .await;
        if let Err(error) = creation {
            let _ = self.cleanup_remote(&lease).await;
            let _ = self.manifests.remove(session_id);
            return Err(error);
        }

        lease.state = RemoteLeaseState::Active;
        if let Err(error) = self.manifests.save(&lease) {
            let _ = self.cleanup_remote(&lease).await;
            let _ = self.manifests.remove(session_id);
            return Err(error);
        }
        Ok(lease)
    }

    /// Load and validate an active local lease manifest.
    pub fn open(&self, session_id: &str) -> Result<RemoteWorkspaceLease, String> {
        self.manifests.open(session_id)
    }

    /// Produce a bounded binary patch, including untracked files.
    pub async fn inspect(&self, lease: &RemoteWorkspaceLease) -> Result<WorkspaceDiff, String> {
        self.validate_remote(lease).await?;
        let status = self
            .git
            .text(&lease.worktree_root, &["status", "--short"])
            .await?;
        let mut patch = self
            .git
            .text(&lease.worktree_root, &["diff", "--binary", "HEAD"])
            .await?;
        let untracked = self
            .git
            .bytes(
                &lease.worktree_root,
                &["ls-files", "--others", "--exclude-standard", "-z"],
                &[0],
            )
            .await?;
        let paths = untracked
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
            .collect::<Vec<_>>();
        if paths.len() > MAX_UNTRACKED_FILES {
            return Err("remote worktree has too many untracked files to review".into());
        }
        for bytes in paths {
            let relative = std::str::from_utf8(bytes)
                .map_err(|_| "remote worktree contains a non-UTF-8 path")?;
            validate_relative(relative)?;
            let output = self
                .git
                .bytes(
                    &lease.worktree_root,
                    &[
                        "diff",
                        "--no-index",
                        "--binary",
                        "--",
                        "/dev/null",
                        relative,
                    ],
                    &[0, 1],
                )
                .await?;
            let addition =
                String::from_utf8(output).map_err(|_| "remote worktree diff is not valid UTF-8")?;
            if !patch.is_empty() {
                patch.push('\n');
            }
            patch.push_str(&addition);
            if patch.len() > MAX_REVIEW_BYTES {
                return Err("remote worktree diff exceeds the review limit".into());
            }
        }
        Ok(WorkspaceDiff { status, patch })
    }

    /// Commit and merge the current remote candidate into its source checkout.
    pub async fn accept(&self, lease: &RemoteWorkspaceLease) -> Result<(), String> {
        let _guard = self.mutation_lock.lock().await;
        self.validate_remote(lease).await?;
        if !self
            .git
            .text(&lease.source_root, &["status", "--porcelain"])
            .await?
            .is_empty()
        {
            return Err("source workspace changed before worktree acceptance".into());
        }
        self.git
            .ok(&lease.worktree_root, &["add", "-A"], true)
            .await?;
        if self
            .git
            .status(&lease.worktree_root, &["diff", "--cached", "--quiet"])
            .await?
            == 0
        {
            return Ok(());
        }
        self.git
            .ok(
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
                true,
            )
            .await?;
        let previous = self
            .git
            .text(&lease.source_root, &["rev-parse", "HEAD"])
            .await?;
        let command = format!(
            "test \"$(git rev-parse HEAD)\" = {} || exit 42\n\
             if ! git merge --no-ff --no-edit {}; then\n\
               git merge --abort >/dev/null 2>&1 || true\n\
               exit 1\n\
             fi",
            shell_quote(&previous),
            shell_quote(&lease.branch),
        );
        self.git
            .command_ok(&lease.source_root, &command, true)
            .await
    }

    /// Remove the remote worktree, branch, and local manifest.
    pub async fn discard(&self, lease: &RemoteWorkspaceLease) -> Result<(), String> {
        let _guard = self.mutation_lock.lock().await;
        self.manifests.validate(lease)?;
        self.cleanup_remote(lease).await?;
        self.manifests.remove(&lease.session_id)
    }

    /// Reconcile manifests with durable sessions after Runtime restart.
    pub async fn reconcile(
        &self,
        active: &HashMap<String, PathBuf>,
    ) -> Result<WorktreeReconciliation, String> {
        let _guard = self.mutation_lock.lock().await;
        let mut remaining = active.clone();
        let mut retained = 0;
        let mut removed = 0;
        let leases = self.manifests.directory();
        if leases.is_dir() {
            let mut manifests = fs::read_dir(&leases)
                .map_err(display_error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(display_error)?;
            manifests.sort_by_key(fs::DirEntry::file_name);
            for entry in manifests {
                if entry.path().extension().and_then(|value| value.to_str()) != Some("json") {
                    continue;
                }
                let lease: RemoteWorkspaceLease =
                    serde_json::from_slice(&fs::read(entry.path()).map_err(display_error)?)
                        .map_err(display_error)?;
                self.manifests.validate(&lease)?;
                let expected = remaining.remove(&lease.session_id);
                if lease.state == RemoteLeaseState::Active
                    && expected.as_ref() == Some(&lease.effective_workspace)
                {
                    self.validate_remote(&lease).await?;
                    retained += 1;
                } else {
                    self.cleanup_remote(&lease).await?;
                    fs::remove_file(entry.path()).map_err(display_error)?;
                    removed += 1;
                }
            }
        }
        if let Some(session_id) = remaining.keys().min() {
            return Err(format!(
                "session {session_id} references a missing remote worktree lease"
            ));
        }
        Ok(WorktreeReconciliation { retained, removed })
    }

    async fn validate_remote(&self, lease: &RemoteWorkspaceLease) -> Result<(), String> {
        self.manifests.validate(lease)?;
        let source = self
            .git
            .text(&lease.source_root, &["rev-parse", "--show-toplevel"])
            .await?;
        let worktree = self
            .git
            .text(&lease.worktree_root, &["rev-parse", "--show-toplevel"])
            .await?;
        let physical_source = self
            .git
            .physical_working_directory(&lease.source_root)
            .await?;
        let physical_worktree = self
            .git
            .physical_working_directory(&lease.worktree_root)
            .await?;
        let branch = self
            .git
            .text(&lease.worktree_root, &["branch", "--show-current"])
            .await?;
        if source != physical_source || worktree != physical_worktree || branch != lease.branch {
            return Err("remote worktree lease does not match its Git repository".into());
        }
        Ok(())
    }

    async fn cleanup_remote(&self, lease: &RemoteWorkspaceLease) -> Result<(), String> {
        let command = format!(
            "git worktree remove --force {} >/dev/null 2>&1 || [ ! -e {} ] || exit 1\n\
             if git show-ref --verify --quiet {}; then\n\
               git branch -D {} >/dev/null\n\
             fi",
            shell_quote(path_text(&lease.worktree_root)?),
            shell_quote(path_text(&lease.worktree_root)?),
            shell_quote(&format!("refs/heads/{}", lease.branch)),
            shell_quote(&lease.branch),
        );
        self.git
            .command_ok(&lease.source_root, &command, true)
            .await
    }
}

fn path_text(path: &Path) -> Result<&str, String> {
    path.to_str()
        .ok_or_else(|| "remote worktree path is not valid UTF-8".into())
}

fn display_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[cfg(test)]
#[path = "../tests/unit/remote_git_worktree.rs"]
mod tests;
