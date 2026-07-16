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

impl PrincipalBindingStore {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, PrincipalBindingError> {
        let path = path.as_ref().to_path_buf();
        Self::open_with(move || Connection::open(path), Arc::new(SystemClock)).await
    }

    #[cfg(test)]
    pub(crate) async fn open_in_memory(
        clock: Arc<dyn Clock>,
    ) -> Result<Self, PrincipalBindingError> {
        Self::open_with(Connection::open_in_memory, clock).await
    }

    async fn open_with(
        open: impl FnOnce() -> rusqlite::Result<Connection> + Send + 'static,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, PrincipalBindingError> {
        let connection = task::spawn_blocking(move || {
            let mut connection = open().map_err(storage)?;
            connection
                .busy_timeout(Duration::from_secs(5))
                .map_err(storage)?;
            connection
                .execute_batch("PRAGMA foreign_keys=ON;")
                .map_err(storage)?;
            initialize_schema(&mut connection)?;
            Ok(connection)
        })
        .await
        .map_err(|_| PrincipalBindingError::Task)??;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            clock,
        })
    }

    async fn run<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Connection) -> Result<T, PrincipalBindingError> + Send + 'static,
    ) -> Result<T, PrincipalBindingError> {
        let connection = self.connection.clone();
        task::spawn_blocking(move || {
            let mut connection = connection.blocking_lock();
            operation(&mut connection)
        })
        .await
        .map_err(|_| PrincipalBindingError::Task)?
    }

    /// Register a stable user identity. User ids are never implicitly created
    /// while linking a channel identity.
    pub async fn register_user(&self, user_id: UserId) -> Result<(), PrincipalBindingError> {
        validate_id("user id", &user_id.0)?;
        if user_id == UserId::system() {
            return Err(PrincipalBindingError::Invalid {
                field: "user id",
                reason: "the system sentinel cannot represent a human user".into(),
            });
        }
        let created_at = self.clock.now();
        self.run(move |connection| {
            connection
                .execute(
                    "INSERT INTO users(user_id,created_at) VALUES (?1,?2)",
                    params![user_id.0, created_at],
                )
                .map_err(|error| {
                    if is_unique_constraint(&error) {
                        PrincipalBindingError::UserAlreadyExists(user_id.0)
                    } else {
                        storage(error)
                    }
                })?;
            Ok(())
        })
        .await
    }

    /// Issue a challenge for an unbound principal and an existing user.
    /// A newer challenge for the same principal invalidates the older one.
    pub async fn begin_link(
        &self,
        principal: ExternalPrincipal,
        user_id: UserId,
        ttl: Duration,
    ) -> Result<IssuedLinkChallenge, PrincipalBindingError> {
        validate_id("user id", &user_id.0)?;
        if !(MIN_CHALLENGE_TTL..=MAX_CHALLENGE_TTL).contains(&ttl) {
            return Err(PrincipalBindingError::Invalid {
                field: "challenge ttl",
                reason: format!(
                    "must be between {} and {} seconds",
                    MIN_CHALLENGE_TTL.as_secs(),
                    MAX_CHALLENGE_TTL.as_secs()
                ),
            });
        }
        let now = self.clock.now();
        let ttl_seconds =
            i64::try_from(ttl.as_secs()).map_err(|_| PrincipalBindingError::Invalid {
                field: "challenge ttl",
                reason: "exceeds the supported range".into(),
            })?;
        let expires_at = now.saturating_add(ttl_seconds);
        let challenge_id = Uuid::new_v4().to_string();
        let secret = Uuid::new_v4().as_simple().to_string();
        let secret_hash = challenge_digest(&challenge_id, &secret);
        let external_digest = principal.digest();
        let result_id = challenge_id.clone();
        let transport = principal.transport;
        let instance = principal.channel_instance_id;
        let target_user = user_id.0;
        self.run(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(storage)?;
            require_user(&transaction, &target_user)?;
            if load_binding(
                &transaction,
                &transport,
                &instance,
                &external_digest,
            )?
            .is_some()
            {
                return Err(PrincipalBindingError::AlreadyLinked);
            }
            transaction
                .execute(
                    "DELETE FROM link_challenges WHERE transport=?1 AND channel_instance_id=?2 AND external_principal_digest=?3",
                    params![transport, instance, external_digest],
                )
                .map_err(storage)?;
            transaction
                .execute(
                    "INSERT INTO link_challenges(challenge_id,transport,channel_instance_id,external_principal_digest,target_user_id,secret_hash,expires_at,attempts,created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,0,?8)",
                    params![challenge_id, transport, instance, external_digest, target_user, secret_hash, expires_at, now],
                )
                .map_err(storage)?;
            transaction.commit().map_err(storage)
        })
        .await?;
        Ok(IssuedLinkChallenge {
            challenge_id: result_id,
            secret: LinkSecret(secret),
            expires_at,
        })
    }

    /// Atomically consume a challenge and create revision one of a binding.
    pub async fn confirm_link(
        &self,
        principal: ExternalPrincipal,
        challenge_id: &str,
        secret: &str,
    ) -> Result<PrincipalBinding, PrincipalBindingError> {
        validate_id("challenge id", challenge_id)?;
        validate_id("challenge secret", secret)?;
        let now = self.clock.now();
        let challenge_id = challenge_id.to_owned();
        let supplied_hash = challenge_digest(&challenge_id, secret);
        let external_digest = principal.digest();
        let transport = principal.transport;
        let instance = principal.channel_instance_id;
        self.run(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(storage)?;
            let challenge = load_challenge(&transaction, &challenge_id)?
                .ok_or(PrincipalBindingError::UnknownChallenge)?;
            if challenge.transport != transport
                || challenge.instance != instance
                || challenge.external_digest != external_digest
            {
                return Err(PrincipalBindingError::ChallengePrincipalMismatch);
            }
            if challenge.expires_at <= now {
                transaction
                    .execute(
                        "DELETE FROM link_challenges WHERE challenge_id=?1",
                        [&challenge_id],
                    )
                    .map_err(storage)?;
                transaction.commit().map_err(storage)?;
                return Err(PrincipalBindingError::ChallengeExpired);
            }
            if !constant_time_eq(challenge.secret_hash.as_bytes(), supplied_hash.as_bytes()) {
                let attempts = challenge.attempts + 1;
                if attempts >= MAX_CONFIRM_ATTEMPTS {
                    transaction
                        .execute(
                            "DELETE FROM link_challenges WHERE challenge_id=?1",
                            [&challenge_id],
                        )
                        .map_err(storage)?;
                    transaction.commit().map_err(storage)?;
                    return Err(PrincipalBindingError::ChallengeLocked);
                }
                transaction
                    .execute(
                        "UPDATE link_challenges SET attempts=?2 WHERE challenge_id=?1 AND attempts=?3",
                        params![challenge_id, attempts, challenge.attempts],
                    )
                    .map_err(storage)?;
                transaction.commit().map_err(storage)?;
                return Err(PrincipalBindingError::InvalidChallengeSecret);
            }
            require_user(&transaction, &challenge.target_user)?;
            if load_binding(&transaction, &transport, &instance, &external_digest)?.is_some()
            {
                return Err(PrincipalBindingError::AlreadyLinked);
            }
            let previous_revision = load_binding_revision(
                &transaction,
                &transport,
                &instance,
                &external_digest,
            )?
            .unwrap_or(0);
            let revision = previous_revision
                .checked_add(1)
                .ok_or(PrincipalBindingError::Storage)?;
            let sql_revision = i64::try_from(revision).map_err(|_| PrincipalBindingError::Storage)?;
            let sql_previous_revision =
                i64::try_from(previous_revision).map_err(|_| PrincipalBindingError::Storage)?;
            transaction
                .execute(
                    "INSERT INTO principal_bindings(transport,channel_instance_id,external_principal_digest,user_id,revision,linked_at,unlinked_at) VALUES (?1,?2,?3,?4,?5,?6,NULL) ON CONFLICT(transport,channel_instance_id,external_principal_digest) DO UPDATE SET user_id=excluded.user_id,revision=excluded.revision,linked_at=excluded.linked_at,unlinked_at=NULL WHERE principal_bindings.user_id IS NULL AND principal_bindings.revision=?7",
                    params![transport, instance, external_digest, challenge.target_user, sql_revision, now, sql_previous_revision],
                )
                .map_err(storage)?;
            transaction
                .execute(
                    "DELETE FROM link_challenges WHERE challenge_id=?1",
                    [&challenge_id],
                )
                .map_err(storage)?;
            let binding = load_binding(&transaction, &transport, &instance, &external_digest)?
                .ok_or(PrincipalBindingError::Storage)?;
            transaction.commit().map_err(storage)?;
            Ok(binding)
        })
        .await
    }

    pub async fn resolve(
        &self,
        principal: ExternalPrincipal,
    ) -> Result<Option<PrincipalBinding>, PrincipalBindingError> {
        let external_digest = principal.digest();
        let transport = principal.transport;
        let instance = principal.channel_instance_id;
        self.run(move |connection| {
            load_binding(connection, &transport, &instance, &external_digest)
        })
        .await
    }

    /// Explicit owner-authorized CAS unlink. A principal cannot be rebound to
    /// another user until its current owner removes the exact revision.
    pub async fn unlink(
        &self,
        principal: ExternalPrincipal,
        owner: &UserId,
        expected_revision: u64,
    ) -> Result<(), PrincipalBindingError> {
        validate_id("user id", &owner.0)?;
        let external_digest = principal.digest();
        let transport = principal.transport;
        let instance = principal.channel_instance_id;
        let owner = owner.0.clone();
        let now = self.clock.now();
        self.run(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(storage)?;
            let existing = load_binding(&transaction, &transport, &instance, &external_digest)?
                .ok_or(PrincipalBindingError::UnknownBinding)?;
            if existing.user_id.0 != owner {
                return Err(PrincipalBindingError::AlreadyLinked);
            }
            if existing.revision != expected_revision {
                return Err(PrincipalBindingError::Conflict {
                    expected: expected_revision,
                    actual: existing.revision,
                });
            }
            let tombstone_revision = expected_revision
                .checked_add(1)
                .ok_or(PrincipalBindingError::Storage)?;
            let sql_expected_revision =
                i64::try_from(expected_revision).map_err(|_| PrincipalBindingError::Storage)?;
            let sql_tombstone_revision =
                i64::try_from(tombstone_revision).map_err(|_| PrincipalBindingError::Storage)?;
            let changed = transaction
                .execute(
                    "UPDATE principal_bindings SET user_id=NULL,revision=?6,unlinked_at=?7 WHERE transport=?1 AND channel_instance_id=?2 AND external_principal_digest=?3 AND user_id=?4 AND revision=?5",
                    params![transport, instance, external_digest, owner, sql_expected_revision, sql_tombstone_revision, now],
                )
                .map_err(storage)?;
            if changed != 1 {
                return Err(PrincipalBindingError::Conflict {
                    expected: expected_revision,
                    actual: existing.revision,
                });
            }
            transaction.commit().map_err(storage)
        })
        .await
    }
}

