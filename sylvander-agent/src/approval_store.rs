//! Durable, content-safe grants for tool approval.
//!
//! A persistent grant is valid only for one stable user, Agent, policy
//! revision, capability revision, operation, and resource fingerprint. The
//! store never persists tool arguments; only domain-separated SHA-256
//! revisions and fingerprints cross the process boundary.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex, OnceLock, Weak};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::approval::{ApprovalRule, RuleAction, ToolUseRequest};
use crate::spec::{AgentId, SessionId};

const STORE_SCHEMA_VERSION: u32 = 1;
const MAX_STORE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_GRANTS: usize = 100_000;

/// Runtime-owned dimensions shared by every approval request in one turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ApprovalGrantContext {
    user_id: String,
    agent_id: AgentId,
    policy_revision: String,
    capability_revision: String,
}

impl ApprovalGrantContext {
    pub(crate) fn new(
        user_id: impl Into<String>,
        agent_id: AgentId,
        policy_revision: String,
        capability_revision: String,
    ) -> Self {
        Self {
            user_id: user_id.into(),
            agent_id,
            policy_revision,
            capability_revision,
        }
    }

    pub(crate) fn key_for(&self, request: &ToolUseRequest) -> ApprovalGrantKey {
        ApprovalGrantKey {
            user_id: self.user_id.clone(),
            agent_id: self.agent_id.0.clone(),
            policy_revision: self.policy_revision.clone(),
            capability_revision: self.capability_revision.clone(),
            operation: request.tool_name.clone(),
            resource_fingerprint: digest_json(b"sylvander.approval.resource.v1\0", &request.input),
        }
    }
}

/// Exact authorization key persisted for a durable approval.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ApprovalGrantKey {
    user_id: String,
    agent_id: String,
    policy_revision: String,
    capability_revision: String,
    operation: String,
    resource_fingerprint: String,
}

impl ApprovalGrantKey {
    fn validate(&self) -> Result<(), String> {
        validate_identity("user id", &self.user_id, 256)?;
        validate_identity("Agent id", &self.agent_id, 256)?;
        validate_identity("operation", &self.operation, 256)?;
        validate_digest("policy revision", &self.policy_revision)?;
        validate_digest("capability revision", &self.capability_revision)?;
        validate_digest("resource fingerprint", &self.resource_fingerprint)
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistentApprovalFile {
    schema_version: u32,
    grants: Vec<ApprovalGrantKey>,
}

/// Session and persistent approval grants owned by one Agent run.
pub(crate) struct ApprovalMemory {
    sessions: HashMap<SessionId, HashSet<ApprovalGrantKey>>,
    persistent: Option<Arc<Mutex<PersistentApprovalState>>>,
}

struct PersistentApprovalState {
    grants: HashSet<ApprovalGrantKey>,
    path: PathBuf,
}

impl ApprovalMemory {
    pub(crate) fn load(path: Option<PathBuf>) -> Result<Self, String> {
        let persistent = match path {
            Some(path) => Some(shared_persistent_state(path)?),
            None => None,
        };
        Ok(Self {
            sessions: HashMap::new(),
            persistent,
        })
    }

    /// Persistent scope is offered only when both durable storage and a
    /// Runtime-authenticated stable identity are available.
    pub(crate) fn allowed_scopes(
        &self,
        persistent_identity_authorized: bool,
    ) -> Vec<sylvander_protocol::ApprovalScope> {
        let mut scopes = vec![
            sylvander_protocol::ApprovalScope::Once,
            sylvander_protocol::ApprovalScope::Session,
        ];
        if self.persistent.is_some() && persistent_identity_authorized {
            scopes.push(sylvander_protocol::ApprovalScope::Persistent);
        }
        scopes
    }

    pub(crate) async fn contains(&self, session_id: &SessionId, key: &ApprovalGrantKey) -> bool {
        if self
            .sessions
            .get(session_id)
            .is_some_and(|entries| entries.contains(key))
        {
            return true;
        }
        match &self.persistent {
            Some(persistent) => persistent.lock().await.grants.contains(key),
            None => false,
        }
    }

    pub(crate) async fn remember(
        &mut self,
        session_id: &SessionId,
        key: ApprovalGrantKey,
        scope: sylvander_protocol::ApprovalScope,
        persistent_identity_authorized: bool,
    ) -> Result<(), String> {
        key.validate()?;
        match scope {
            sylvander_protocol::ApprovalScope::Once => Ok(()),
            sylvander_protocol::ApprovalScope::Session => {
                self.sessions
                    .entry(session_id.clone())
                    .or_default()
                    .insert(key);
                Ok(())
            }
            sylvander_protocol::ApprovalScope::Persistent => {
                if !persistent_identity_authorized {
                    return Err(
                        "persistent approval requires a Runtime-authenticated stable identity"
                            .into(),
                    );
                }
                let persistent = self.persistent.clone().ok_or_else(|| {
                    "persistent approvals are disabled by the operator".to_string()
                })?;
                let mut persistent = persistent.lock().await;
                let inserted = persistent.grants.insert(key.clone());
                if let Err(error) =
                    persist_approval_grants(&persistent.path, &persistent.grants).await
                {
                    if inserted {
                        persistent.grants.remove(&key);
                    }
                    return Err(error);
                }
                Ok(())
            }
        }
    }

    pub(crate) fn remove_session(&mut self, session_id: &SessionId) {
        self.sessions.remove(session_id);
    }
}

fn shared_persistent_state(path: PathBuf) -> Result<Arc<Mutex<PersistentApprovalState>>, String> {
    type Registry = HashMap<PathBuf, Weak<Mutex<PersistentApprovalState>>>;
    static REGISTRY: OnceLock<StdMutex<Registry>> = OnceLock::new();

    let registry = REGISTRY.get_or_init(|| StdMutex::new(HashMap::new()));
    let mut registry = registry
        .lock()
        .map_err(|_| "approval store registry is unavailable".to_string())?;
    if let Some(existing) = registry.get(&path).and_then(Weak::upgrade) {
        return Ok(existing);
    }
    let grants = if path.exists() {
        load_persistent_grants(&path)?
    } else {
        HashSet::new()
    };
    let state = Arc::new(Mutex::new(PersistentApprovalState {
        grants,
        path: path.clone(),
    }));
    registry.insert(path, Arc::downgrade(&state));
    Ok(state)
}

/// Content-addressed revision of the effective approval policy.
pub(crate) fn approval_policy_revision(
    permissions: &sylvander_protocol::PermissionProfile,
    rules: &[ApprovalRule],
) -> String {
    let rules = rules
        .iter()
        .map(|rule| {
            let action = match &rule.action {
                RuleAction::AutoApprove => serde_json::json!({"kind": "approve"}),
                RuleAction::AutoReject { reason } => {
                    serde_json::json!({"kind": "reject", "reason": reason})
                }
            };
            serde_json::json!({"tools": rule.tools, "action": action})
        })
        .collect::<Vec<_>>();
    digest_json(
        b"sylvander.approval.policy.v1\0",
        &serde_json::json!({"permissions": permissions, "rules": rules}),
    )
}

fn load_persistent_grants(path: &Path) -> Result<HashSet<ApprovalGrantKey>, String> {
    let metadata = std::fs::metadata(path).map_err(|error| {
        format!(
            "failed to inspect approval store {}: {error}",
            path.display()
        )
    })?;
    if metadata.len() > MAX_STORE_BYTES {
        return Err(format!(
            "approval store {} exceeds the {} byte limit",
            path.display(),
            MAX_STORE_BYTES
        ));
    }
    let bytes = std::fs::read(path)
        .map_err(|error| format!("failed to read approval store {}: {error}", path.display()))?;
    let file = serde_json::from_slice::<PersistentApprovalFile>(&bytes)
        .map_err(|error| format!("failed to parse approval store {}: {error}", path.display()))?;
    if file.schema_version != STORE_SCHEMA_VERSION {
        return Err(format!(
            "unsupported approval store schema {} (expected {})",
            file.schema_version, STORE_SCHEMA_VERSION
        ));
    }
    if file.grants.len() > MAX_GRANTS {
        return Err(format!(
            "approval store exceeds the {MAX_GRANTS} grant limit"
        ));
    }
    let mut grants = HashSet::with_capacity(file.grants.len());
    for grant in file.grants {
        grant.validate()?;
        if !grants.insert(grant) {
            return Err("approval store contains a duplicate grant".into());
        }
    }
    Ok(grants)
}

async fn persist_approval_grants(
    path: &Path,
    grants: &HashSet<ApprovalGrantKey>,
) -> Result<(), String> {
    let path = path.to_owned();
    let mut grants = grants.iter().cloned().collect::<Vec<_>>();
    grants.sort();
    tokio::task::spawn_blocking(move || persist_approval_grants_sync(&path, grants))
        .await
        .map_err(|error| format!("approval store writer failed: {error}"))?
}

fn persist_approval_grants_sync(path: &Path, grants: Vec<ApprovalGrantKey>) -> Result<(), String> {
    use std::io::Write as _;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create approval store directory: {error}"))?;
    }
    let bytes = serde_json::to_vec_pretty(&PersistentApprovalFile {
        schema_version: STORE_SCHEMA_VERSION,
        grants,
    })
    .map_err(|error| format!("failed to encode approval store: {error}"))?;
    let temporary = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    let write_result = (|| -> Result<(), String> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .map_err(|error| format!("failed to create approval store: {error}"))?;
        file.write_all(&bytes)
            .map_err(|error| format!("failed to write approval store: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("failed to sync approval store: {error}"))?;
        std::fs::rename(&temporary, path)
            .map_err(|error| format!("failed to replace approval store: {error}"))?;
        if let Some(parent) = path.parent() {
            std::fs::File::open(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(|error| format!("failed to sync approval store directory: {error}"))?;
        }
        Ok(())
    })();
    if write_result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    write_result
}

fn digest_json(domain: &[u8], value: &serde_json::Value) -> String {
    let canonical = canonical_json(value);
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(serde_json::to_vec(&canonical).unwrap_or_default());
    format!("sha256:{:x}", hasher.finalize())
}

fn canonical_json(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(canonical_json).collect())
        }
        serde_json::Value::Object(map) => {
            let mut entries = map.iter().collect::<Vec<_>>();
            entries.sort_by(|left, right| left.0.cmp(right.0));
            let mut canonical = serde_json::Map::new();
            for (key, value) in entries {
                canonical.insert(key.clone(), canonical_json(value));
            }
            serde_json::Value::Object(canonical)
        }
        scalar => scalar.clone(),
    }
}

fn validate_identity(label: &str, value: &str, max_bytes: usize) -> Result<(), String> {
    if value.is_empty() || value.len() > max_bytes || value.chars().any(char::is_control) {
        return Err(format!("invalid approval {label}"));
    }
    Ok(())
}

fn validate_digest(label: &str, value: &str) -> Result<(), String> {
    let digest = value.strip_prefix("sha256:").unwrap_or_default();
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(format!("invalid approval {label}"));
    }
    Ok(())
}

#[cfg(test)]
#[path = "../tests/unit/approval_store.rs"]
mod tests;
