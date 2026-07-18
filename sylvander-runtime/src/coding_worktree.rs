//! Target-aware coding-session worktree orchestration.
//!
//! Local, container, and sandbox workspaces use the server-side Git manager.
//! SSH workspaces use a manager bound to that target's executor. Runtime calls
//! this service without needing to know where the repository physically lives.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::git_worktree::{GitWorktreeManager, WorkspaceDiff, WorktreeReconciliation};
use crate::remote_git_worktree::RemoteGitWorktreeManager;

/// Metadata Runtime persists with a session after creating an isolated branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodingWorkspaceLease {
    pub effective_workspace: PathBuf,
    pub branch: String,
    pub target_id: Option<String>,
}

/// Durable session reference used during startup reconciliation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveCodingWorkspace {
    pub session_id: String,
    pub effective_workspace: PathBuf,
    pub target_id: Option<String>,
}

/// Aggregates server-local and executor-backed worktree managers.
#[derive(Clone)]
pub struct CodingWorktreeService {
    local: Arc<GitWorktreeManager>,
    local_targets: HashSet<String>,
    remote: HashMap<String, Arc<RemoteGitWorktreeManager>>,
}

impl CodingWorktreeService {
    #[must_use]
    pub fn new(local: Arc<GitWorktreeManager>) -> Self {
        Self {
            local,
            local_targets: HashSet::new(),
            remote: HashMap::new(),
        }
    }

    /// Register a target whose workspace paths live on the server host.
    pub fn register_local(&mut self, target_id: impl Into<String>) -> Result<(), String> {
        let target_id = target_id.into();
        if self.remote.contains_key(&target_id) || !self.local_targets.insert(target_id.clone()) {
            return Err(format!("duplicate coding worktree target {target_id}"));
        }
        Ok(())
    }

    /// Register the manager for one configured remote execution target.
    pub fn register_remote(
        &mut self,
        target_id: impl Into<String>,
        manager: Arc<RemoteGitWorktreeManager>,
    ) -> Result<(), String> {
        let target_id = target_id.into();
        if self.local_targets.contains(&target_id)
            || self.remote.insert(target_id.clone(), manager).is_some()
        {
            return Err(format!("duplicate coding worktree target {target_id}"));
        }
        Ok(())
    }

    /// Create an isolated branch when the selected workspace is a Git checkout.
    ///
    /// A non-Git workspace is valid and returns `None`; transport or Git
    /// failures return an error rather than silently falling back to the server.
    pub async fn create(
        &self,
        session_id: &str,
        target_id: &str,
        requested: &Path,
    ) -> Result<Option<CodingWorkspaceLease>, String> {
        if let Some(manager) = self.remote.get(target_id) {
            if !manager.is_git_workspace(requested).await {
                return Ok(None);
            }
            let lease = manager.create(session_id, requested).await?;
            return Ok(Some(CodingWorkspaceLease {
                effective_workspace: lease.effective_workspace,
                branch: lease.branch,
                target_id: Some(target_id.to_owned()),
            }));
        }
        if !self.local_targets.contains(target_id) {
            return Err(format!("unknown coding worktree target {target_id}"));
        }

        let manager = self.local.clone();
        let requested = requested.to_owned();
        let session_id = session_id.to_owned();
        tokio::task::spawn_blocking(move || {
            if !manager.is_git_workspace(&requested) {
                return Ok(None);
            }
            manager.create(&session_id, &requested).map(|lease| {
                Some(CodingWorkspaceLease {
                    effective_workspace: lease.effective_workspace,
                    branch: lease.branch,
                    target_id: None,
                })
            })
        })
        .await
        .map_err(|_| "worktree creation stopped".to_string())?
    }

    /// Inspect tracked and untracked changes for one durable session lease.
    pub async fn inspect(
        &self,
        session_id: &str,
        target_id: Option<&str>,
    ) -> Result<WorkspaceDiff, String> {
        if let Some(manager) = self.remote_manager(target_id)? {
            let lease = manager.open(session_id)?;
            return manager.inspect(&lease).await;
        }
        let manager = self.local.clone();
        let session_id = session_id.to_owned();
        tokio::task::spawn_blocking(move || {
            let lease = manager.open(&session_id)?;
            manager.inspect(&lease)
        })
        .await
        .map_err(|_| "worktree inspection stopped".to_string())?
    }

    /// Commit and merge one reviewed candidate into its source checkout.
    pub async fn accept(&self, session_id: &str, target_id: Option<&str>) -> Result<(), String> {
        if let Some(manager) = self.remote_manager(target_id)? {
            let lease = manager.open(session_id)?;
            return manager.accept(&lease).await;
        }
        let manager = self.local.clone();
        let session_id = session_id.to_owned();
        tokio::task::spawn_blocking(move || {
            let lease = manager.open(&session_id)?;
            manager.accept(&lease)
        })
        .await
        .map_err(|_| "worktree merge stopped".to_string())?
    }

    /// Remove one worktree, branch, and manifest.
    pub async fn discard(&self, session_id: &str, target_id: Option<&str>) -> Result<(), String> {
        if let Some(manager) = self.remote_manager(target_id)? {
            let lease = manager.open(session_id)?;
            return manager.discard(&lease).await;
        }
        let manager = self.local.clone();
        let session_id = session_id.to_owned();
        tokio::task::spawn_blocking(move || {
            let lease = manager.open(&session_id)?;
            manager.discard(&lease)
        })
        .await
        .map_err(|_| "worktree discard stopped".to_string())?
    }

    /// Best-effort compensation for a session that failed before persistence.
    pub async fn discard_if_present(
        &self,
        session_id: &str,
        target_id: Option<&str>,
    ) -> Result<(), String> {
        if let Some(manager) = self.remote_manager(target_id)? {
            return match manager.open(session_id) {
                Ok(lease) => manager.discard(&lease).await,
                Err(_) => Ok(()),
            };
        }
        let manager = self.local.clone();
        let session_id = session_id.to_owned();
        tokio::task::spawn_blocking(move || match manager.open(&session_id) {
            Ok(lease) => manager.discard(&lease),
            Err(_) => Ok(()),
        })
        .await
        .map_err(|_| "worktree compensation stopped".to_string())?
    }

    /// Reconcile all local and remote manifests with restored sessions.
    pub async fn reconcile(
        &self,
        active: &[ActiveCodingWorkspace],
    ) -> Result<WorktreeReconciliation, String> {
        let mut local = HashMap::new();
        let mut remote = self
            .remote
            .keys()
            .map(|target| (target.clone(), HashMap::new()))
            .collect::<HashMap<_, _>>();
        for session in active {
            match session.target_id.as_ref() {
                Some(target) => {
                    let sessions = remote
                        .get_mut(target)
                        .ok_or_else(|| format!("unknown remote worktree target {target}"))?;
                    sessions.insert(
                        session.session_id.clone(),
                        session.effective_workspace.clone(),
                    );
                }
                None => {
                    local.insert(
                        session.session_id.clone(),
                        session.effective_workspace.clone(),
                    );
                }
            }
        }

        let manager = self.local.clone();
        let mut total = tokio::task::spawn_blocking(move || manager.reconcile(&local))
            .await
            .map_err(|_| "local worktree reconciliation stopped".to_string())??;
        for (target, sessions) in remote {
            let reconciled = self
                .remote
                .get(&target)
                .expect("target came from manager map")
                .reconcile(&sessions)
                .await?;
            total.retained += reconciled.retained;
            total.removed += reconciled.removed;
        }
        Ok(total)
    }

    fn remote_manager(
        &self,
        target_id: Option<&str>,
    ) -> Result<Option<&Arc<RemoteGitWorktreeManager>>, String> {
        let Some(target_id) = target_id else {
            return Ok(None);
        };
        self.remote
            .get(target_id)
            .map(Some)
            .ok_or_else(|| format!("unknown remote worktree target {target_id}"))
    }
}

#[cfg(test)]
#[path = "../tests/unit/coding_worktree.rs"]
mod tests;
