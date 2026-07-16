//! Stable user identities and external-channel principal bindings.
//!
//! External identities are scoped by transport and channel instance. Their
//! raw identifiers are never persisted: the store retains only a
//! domain-separated SHA-256 digest. A binding can only be created by
//! confirming a short-lived, single-use challenge delivered through that
//! exact external principal.

use std::fmt;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use sylvander_protocol::UserId;
use tokio::sync::Mutex;
use tokio::task;
use uuid::Uuid;

const MAX_ID_BYTES: usize = 512;
const MIN_CHALLENGE_TTL: Duration = Duration::from_secs(30);
const MAX_CHALLENGE_TTL: Duration = Duration::from_mins(15);
const MAX_CONFIRM_ATTEMPTS: i64 = 5;
const SCHEMA_VERSION: i64 = 1;
const APPLICATION_ID: i64 = 0x5359_5042;

/// One principal in one concrete channel deployment.
#[derive(Clone, PartialEq, Eq)]
pub struct ExternalPrincipal {
    pub transport: String,
    pub channel_instance_id: String,
    external_id: String,
}

impl fmt::Debug for ExternalPrincipal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExternalPrincipal")
            .field("transport", &self.transport)
            .field("channel_instance_id", &self.channel_instance_id)
            .field("external_id", &"[REDACTED]")
            .finish()
    }
}

impl ExternalPrincipal {
    pub fn new(
        transport: impl Into<String>,
        channel_instance_id: impl Into<String>,
        external_id: impl Into<String>,
    ) -> Result<Self, PrincipalBindingError> {
        let principal = Self {
            transport: transport.into(),
            channel_instance_id: channel_instance_id.into(),
            external_id: external_id.into(),
        };
        validate_id("transport", &principal.transport)?;
        validate_id("channel instance", &principal.channel_instance_id)?;
        validate_id("external principal", &principal.external_id)?;
        Ok(principal)
    }

    fn digest(&self) -> String {
        digest_parts(&[
            b"sylvander.external-principal.v1",
            self.transport.as_bytes(),
            self.channel_instance_id.as_bytes(),
            self.external_id.as_bytes(),
        ])
    }
}

/// A durable external-principal to Sylvander-user binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrincipalBinding {
    pub transport: String,
    pub channel_instance_id: String,
    pub user_id: UserId,
    pub revision: u64,
    pub linked_at: i64,
}

/// Secret returned exactly once when a link challenge is issued.
pub struct LinkSecret(String);

impl LinkSecret {
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for LinkSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("LinkSecret([REDACTED])")
    }
}

/// One pending challenge. The caller must deliver `secret` to the exact
/// external principal and later return it to `confirm_link`.
#[derive(Debug)]
pub struct IssuedLinkChallenge {
    pub challenge_id: String,
    pub secret: LinkSecret,
    pub expires_at: i64,
}

#[derive(Debug, thiserror::Error)]
pub enum PrincipalBindingError {
    #[error("invalid {field}: {reason}")]
    Invalid { field: &'static str, reason: String },
    #[error("user `{0}` already exists")]
    UserAlreadyExists(String),
    #[error("unknown user `{0}`")]
    UnknownUser(String),
    #[error("external principal is already linked")]
    AlreadyLinked,
    #[error("external principal is not linked")]
    UnknownBinding,
    #[error("unknown link challenge")]
    UnknownChallenge,
    #[error("link challenge does not belong to this external principal")]
    ChallengePrincipalMismatch,
    #[error("link challenge expired")]
    ChallengeExpired,
    #[error("link challenge secret is invalid")]
    InvalidChallengeSecret,
    #[error("link challenge attempt limit reached")]
    ChallengeLocked,
    #[error("binding revision conflict: expected {expected}, found {actual}")]
    Conflict { expected: u64, actual: u64 },
    #[error("principal binding database schema is incompatible")]
    IncompatibleSchema,
    #[error("principal binding storage failed")]
    Storage,
    #[error("principal binding task failed")]
    Task,
}

pub(crate) trait Clock: Send + Sync {
    fn now(&self) -> i64;
}

struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| {
                i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
            })
    }
}

/// Latest-schema `SQLite` store for stable users and principal bindings.
#[derive(Clone)]
pub struct PrincipalBindingStore {
    connection: Arc<Mutex<Connection>>,
    clock: Arc<dyn Clock>,
}

fn validate_id(field: &'static str, value: &str) -> Result<(), PrincipalBindingError> {
    if value.is_empty() || value.trim() != value {
        return Err(PrincipalBindingError::Invalid {
            field,
            reason: "must be non-empty and have no surrounding whitespace".into(),
        });
    }
    if value.len() > MAX_ID_BYTES || value.chars().any(char::is_control) {
        return Err(PrincipalBindingError::Invalid {
            field,
            reason: "is too long or contains control characters".into(),
        });
    }
    Ok(())
}

fn digest_parts(parts: &[&[u8]]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part);
    }
    format!("{:x}", hasher.finalize())
}

