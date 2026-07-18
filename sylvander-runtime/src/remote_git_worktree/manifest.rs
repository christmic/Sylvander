//! Local durable intent store for executor-backed worktree leases.

use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteWorkspaceLease {
    pub session_id: String,
    pub target_id: String,
    pub source_root: PathBuf,
    pub worktree_root: PathBuf,
    pub effective_workspace: PathBuf,
    pub branch: String,
    pub(super) state: RemoteLeaseState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum RemoteLeaseState {
    Creating,
    Active,
}

#[derive(Debug, Clone)]
pub(super) struct LeaseManifestStore {
    state_root: PathBuf,
    remote_root: PathBuf,
    target_id: String,
}

impl LeaseManifestStore {
    pub(super) fn new(
        state_root: PathBuf,
        remote_root: PathBuf,
        target_id: String,
    ) -> Self {
        Self {
            state_root,
            remote_root,
            target_id,
        }
    }

    pub(super) fn open(&self, session_id: &str) -> Result<RemoteWorkspaceLease, String> {
        validate_session_id(session_id)?;
        let bytes = fs::read(self.path(session_id)).map_err(display_error)?;
        let lease: RemoteWorkspaceLease = serde_json::from_slice(&bytes).map_err(display_error)?;
        self.validate(&lease)?;
        if lease.state != RemoteLeaseState::Active {
            return Err("remote worktree lease is not active".into());
        }
        Ok(lease)
    }

    pub(super) fn save(&self, lease: &RemoteWorkspaceLease) -> Result<(), String> {
        let path = self.path(&lease.session_id);
        fs::create_dir_all(path.parent().expect("manifest has parent")).map_err(display_error)?;
        let temporary = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
        fs::write(
            &temporary,
            serde_json::to_vec(lease).map_err(display_error)?,
        )
        .map_err(display_error)?;
        fs::rename(temporary, path).map_err(display_error)
    }

    pub(super) fn remove(&self, session_id: &str) -> Result<(), String> {
        let path = self.path(session_id);
        if path.exists() {
            fs::remove_file(path).map_err(display_error)?;
        }
        Ok(())
    }

    pub(super) fn path(&self, session_id: &str) -> PathBuf {
        self.state_root
            .join("leases")
            .join(format!("{session_id}.json"))
    }

    pub(super) fn directory(&self) -> PathBuf {
        self.state_root.join("leases")
    }

    pub(super) fn validate(&self, lease: &RemoteWorkspaceLease) -> Result<(), String> {
        validate_session_id(&lease.session_id)?;
        if lease.target_id != self.target_id
            || lease.branch != format!("sylvander/{}", lease.session_id)
        {
            return Err("invalid remote worktree lease identity".into());
        }
        for path in [
            &lease.source_root,
            &lease.worktree_root,
            &lease.effective_workspace,
        ] {
            validate_remote_absolute(path)?;
        }
        if !lease.worktree_root.starts_with(&self.remote_root)
            || !lease.effective_workspace.starts_with(&lease.worktree_root)
        {
            return Err("remote worktree lease escapes its managed directory".into());
        }
        Ok(())
    }
}

pub(super) fn validate_session_id(value: &str) -> Result<(), String> {
    if !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        Ok(())
    } else {
        Err("session id is not safe for a worktree branch".into())
    }
}

pub(super) fn validate_remote_absolute(path: &Path) -> Result<(), String> {
    if !path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::CurDir | Component::ParentDir | Component::Prefix(_)
            )
        })
        || path.to_str().is_none()
    {
        return Err("remote worktree path must be absolute, normalized, and UTF-8".into());
    }
    Ok(())
}

pub(super) fn validate_relative(path: &str) -> Result<(), String> {
    let path = Path::new(path);
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::CurDir
                    | Component::ParentDir
                    | Component::RootDir
                    | Component::Prefix(_)
            )
        })
    {
        return Err("remote worktree contains an unsafe relative path".into());
    }
    Ok(())
}

fn display_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}
