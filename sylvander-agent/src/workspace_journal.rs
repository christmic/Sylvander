//! Provider-neutral port for journaling workspace mutations.
//!
//! Agent tools participate in a two-phase mutation protocol: they ask the
//! Runtime to prepare durable rollback state before writing, then commit the
//! opaque handle after the executor confirms the write. The Agent deliberately
//! knows nothing about manifests, files, databases, or recovery policy.

use std::fmt::Debug;
use std::path::Path;

const MAX_RUNTIME_TOKEN_BYTES: usize = 4_096;

/// Opaque Runtime-issued handle for one prepared workspace mutation.
///
/// The token is meaningful only to the injected journal implementation. It is
/// not a wire identifier and must never be accepted from model input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedMutation {
    token: String,
}

impl PreparedMutation {
    /// Construct a handle at the Runtime implementation boundary.
    pub fn from_runtime_token(token: impl Into<String>) -> Result<Self, String> {
        let token = token.into();
        if token.is_empty() {
            return Err("workspace mutation token cannot be empty".into());
        }
        if token.len() > MAX_RUNTIME_TOKEN_BYTES {
            return Err("workspace mutation token exceeds 4096 bytes".into());
        }
        if token.chars().any(char::is_control) {
            return Err("workspace mutation token cannot contain control characters".into());
        }
        Ok(Self { token })
    }

    /// Return the opaque token to the Runtime implementation that issued it.
    #[must_use]
    pub fn runtime_token(&self) -> &str {
        &self.token
    }
}

/// Runtime-owned persistence port used by workspace mutation tools.
///
/// Implementations must durably capture rollback state before returning from
/// [`Self::prepare`] and must reject [`Self::commit`] when the post-write
/// content no longer matches the prepared mutation.
pub trait WorkspaceMutationJournal: Debug + Send + Sync {
    /// Prepare rollback state for a write that has not happened yet.
    fn prepare(
        &self,
        session_id: &str,
        turn_id: &str,
        workspace: &Path,
        relative_path: &str,
        after: &[u8],
    ) -> Result<PreparedMutation, String>;

    /// Mark a prepared mutation as applied after the workspace executor writes.
    fn commit(&self, prepared: &PreparedMutation) -> Result<(), String>;
}

#[cfg(test)]
#[path = "../tests/unit/workspace_journal.rs"]
mod tests;
