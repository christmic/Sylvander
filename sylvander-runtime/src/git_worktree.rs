//! Git worktree lifecycle used to isolate coding sessions.

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
        git_ok(&lease.worktree_root, &["add", "-A"])?;
        let staged = git_output(&lease.worktree_root, &["diff", "--cached", "--quiet"])?;
        if !staged.status.success() {
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
            git_ok(&lease.source_root, &["merge", "--no-edit", &lease.branch])?;
        }
        Ok(())
    }

    pub fn discard(&self, lease: &WorkspaceLease) -> Result<(), String> {
        self.cleanup(lease)
    }

    fn cleanup(&self, lease: &WorkspaceLease) -> Result<(), String> {
        git_ok(
            &lease.source_root,
            &[
                "worktree",
                "remove",
                "--force",
                path_text(&lease.worktree_root)?,
            ],
        )?;
        git_ok(&lease.source_root, &["branch", "-D", &lease.branch])?;
        fs::remove_file(self.manifest_path(&lease.session_id)).map_err(display_error)
    }

    fn save(&self, lease: &WorkspaceLease) -> Result<(), String> {
        let path = self.manifest_path(&lease.session_id);
        fs::create_dir_all(path.parent().expect("manifest has parent")).map_err(display_error)?;
        fs::write(path, serde_json::to_vec(lease).map_err(display_error)?).map_err(display_error)
    }

    fn manifest_path(&self, session_id: &str) -> PathBuf {
        self.base.join("leases").join(format!("{session_id}.json"))
    }
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
}
