//! Stable user identities and external-channel principal bindings.
//!
//! External identities are scoped by transport and channel instance. Their
//! raw identifiers are never persisted: the store retains only a
//! HMAC-keyed digest. A binding can only be created when a trusted stable-user
//! ingress issues a short-lived, single-use challenge and the target external
//! principal proves possession of it.

use std::fmt::{self, Write as _};
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
const KEY_VERIFIER_CONTEXT: &[u8] = b"sylvander.principal-binding-key.v1";

/// Secret material used to make persisted principal digests resistant to
/// offline enumeration. Runtime must resolve it outside the database.
pub struct PrincipalDigestKey(Vec<u8>);

impl PrincipalDigestKey {
    pub fn new(key: &[u8]) -> Result<Self, PrincipalBindingError> {
        if !(32..=4096).contains(&key.len()) {
            return Err(PrincipalBindingError::Invalid {
                field: "principal digest key",
                reason: "must contain between 32 and 4096 bytes".into(),
            });
        }
        Ok(Self(key.to_vec()))
    }
}

impl fmt::Debug for PrincipalDigestKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PrincipalDigestKey([REDACTED])")
    }
}

impl Drop for PrincipalDigestKey {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

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

    fn digest(&self, key: &[u8]) -> String {
        hmac_parts(
            key,
            &[
                b"sylvander.external-principal.v1",
                self.transport.as_bytes(),
                self.channel_instance_id.as_bytes(),
                self.external_id.as_bytes(),
            ],
        )
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

/// One pending challenge issued to an authenticated stable user. The user
/// carries `secret` to the external Channel that should become linked.
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
    digest_key: Arc<PrincipalDigestKey>,
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

fn digest_parts(parts: &[&[u8]]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part);
    }
    hasher.finalize().into()
}

fn hex_digest(parts: &[&[u8]]) -> String {
    encode_hex(&digest_parts(parts))
}

fn encode_hex(bytes: &[u8]) -> String {
    bytes.iter().fold(
        String::with_capacity(bytes.len() * 2),
        |mut output, byte| {
            write!(output, "{byte:02x}").expect("writing to String cannot fail");
            output
        },
    )
}

fn hmac_parts(key: &[u8], parts: &[&[u8]]) -> String {
    const BLOCK_SIZE: usize = 64;
    let mut block = [0_u8; BLOCK_SIZE];
    if key.len() > BLOCK_SIZE {
        block[..32].copy_from_slice(&digest_parts(&[key]));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner_pad = [0x36_u8; BLOCK_SIZE];
    let mut outer_pad = [0x5c_u8; BLOCK_SIZE];
    for index in 0..BLOCK_SIZE {
        inner_pad[index] ^= block[index];
        outer_pad[index] ^= block[index];
    }
    let mut inner_hasher = Sha256::new();
    inner_hasher.update(inner_pad);
    for part in parts {
        inner_hasher.update((part.len() as u64).to_be_bytes());
        inner_hasher.update(part);
    }
    let inner = inner_hasher.finalize();
    let mut outer_hasher = Sha256::new();
    outer_hasher.update(outer_pad);
    outer_hasher.update(inner);
    let result = outer_hasher.finalize();
    block.fill(0);
    inner_pad.fill(0);
    outer_pad.fill(0);
    encode_hex(&result)
}

impl PrincipalBindingStore {
    pub async fn open(
        path: impl AsRef<Path>,
        digest_key: PrincipalDigestKey,
    ) -> Result<Self, PrincipalBindingError> {
        let path = path.as_ref().to_path_buf();
        Self::open_with(
            move || Connection::open(path),
            Arc::new(SystemClock),
            digest_key,
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn open_in_memory(
        clock: Arc<dyn Clock>,
        digest_key: PrincipalDigestKey,
    ) -> Result<Self, PrincipalBindingError> {
        Self::open_with(Connection::open_in_memory, clock, digest_key).await
    }

    async fn open_with(
        open: impl FnOnce() -> rusqlite::Result<Connection> + Send + 'static,
        clock: Arc<dyn Clock>,
        digest_key: PrincipalDigestKey,
    ) -> Result<Self, PrincipalBindingError> {
        let digest_key = Arc::new(digest_key);
        let schema_key = digest_key.clone();
        let connection = task::spawn_blocking(move || {
            let mut connection = open().map_err(storage)?;
            connection
                .busy_timeout(Duration::from_secs(5))
                .map_err(storage)?;
            connection
                .execute_batch("PRAGMA foreign_keys=ON;")
                .map_err(storage)?;
            initialize_schema(&mut connection, &schema_key.0)?;
            Ok(connection)
        })
        .await
        .map_err(|_| PrincipalBindingError::Task)??;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            clock,
            digest_key,
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

    fn principal_digest(&self, principal: &ExternalPrincipal) -> String {
        principal.digest(&self.digest_key.0)
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

    /// Issue a challenge for an existing, already authenticated stable user.
    /// A newer challenge for the same user invalidates the older one.
    pub async fn begin_link(
        &self,
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
        let result_id = challenge_id.clone();
        let target_user = user_id.0;
        self.run(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(storage)?;
            require_user(&transaction, &target_user)?;
            transaction
                .execute(
                    "DELETE FROM link_challenges WHERE target_user_id=?1",
                    [&target_user],
                )
                .map_err(storage)?;
            transaction
                .execute(
                    "INSERT INTO link_challenges(challenge_id,target_user_id,secret_hash,expires_at,attempts,created_at) VALUES (?1,?2,?3,?4,0,?5)",
                    params![challenge_id, target_user, secret_hash, expires_at, now],
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
        let external_digest = self.principal_digest(&principal);
        let transport = principal.transport;
        let instance = principal.channel_instance_id;
        self.run(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(storage)?;
            let challenge = load_challenge(&transaction, &challenge_id)?
                .ok_or(PrincipalBindingError::UnknownChallenge)?;
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
        let external_digest = self.principal_digest(&principal);
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
        let external_digest = self.principal_digest(&principal);
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

struct StoredChallenge {
    target_user: String,
    secret_hash: String,
    expires_at: i64,
    attempts: i64,
}

fn initialize_schema(
    connection: &mut Connection,
    digest_key: &[u8],
) -> Result<(), PrincipalBindingError> {
    let version = connection
        .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
        .map_err(storage)?;
    let object_count = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map_err(storage)?;
    let application_id = connection
        .query_row("PRAGMA application_id", [], |row| row.get::<_, i64>(0))
        .map_err(storage)?;
    match (version, object_count, application_id) {
        (0, 0, 0) => {
            connection.execute_batch(SCHEMA).map_err(storage)?;
            let verifier = hmac_parts(digest_key, &[KEY_VERIFIER_CONTEXT]);
            connection
                .execute(
                    "INSERT INTO principal_binding_metadata(singleton,key_verifier) VALUES (1,?1)",
                    [verifier],
                )
                .map_err(storage)?;
        }
        (SCHEMA_VERSION, _, APPLICATION_ID) => validate_schema(connection)?,
        _ => return Err(PrincipalBindingError::IncompatibleSchema),
    }
    let stored_verifier = connection
        .query_row(
            "SELECT key_verifier FROM principal_binding_metadata WHERE singleton=1",
            [],
            |row| row.get::<_, String>(0),
        )
        .map_err(storage)?;
    let expected_verifier = hmac_parts(digest_key, &[KEY_VERIFIER_CONTEXT]);
    if !constant_time_eq(stored_verifier.as_bytes(), expected_verifier.as_bytes()) {
        return Err(PrincipalBindingError::Storage);
    }
    connection
        .query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0))
        .map_err(storage)
        .and_then(|result| {
            if result == "ok" {
                Ok(())
            } else {
                Err(PrincipalBindingError::Storage)
            }
        })
}

fn validate_schema(connection: &Connection) -> Result<(), PrincipalBindingError> {
    let expected = Connection::open_in_memory().map_err(storage)?;
    expected.execute_batch(SCHEMA).map_err(storage)?;
    if schema_objects(connection)? != schema_objects(&expected)? {
        return Err(PrincipalBindingError::IncompatibleSchema);
    }
    for (table, expected_columns) in [
        (
            "principal_binding_metadata",
            &["singleton", "key_verifier"][..],
        ),
        ("users", &["user_id", "created_at"][..]),
        (
            "principal_bindings",
            &[
                "transport",
                "channel_instance_id",
                "external_principal_digest",
                "user_id",
                "revision",
                "linked_at",
                "unlinked_at",
            ][..],
        ),
        (
            "link_challenges",
            &[
                "challenge_id",
                "target_user_id",
                "secret_hash",
                "expires_at",
                "attempts",
                "created_at",
            ][..],
        ),
    ] {
        let exists = connection
            .query_row(
                "SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1",
                [table],
                |_| Ok(()),
            )
            .optional()
            .map_err(storage)?
            .is_some();
        if !exists {
            return Err(PrincipalBindingError::IncompatibleSchema);
        }
        let mut statement = connection
            .prepare(&format!("PRAGMA table_xinfo('{table}')"))
            .map_err(storage)?;
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(storage)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(storage)?;
        if columns != expected_columns {
            return Err(PrincipalBindingError::IncompatibleSchema);
        }
    }
    Ok(())
}

fn schema_objects(
    connection: &Connection,
) -> Result<Vec<(String, String, String, String)>, PrincipalBindingError> {
    let mut statement = connection
        .prepare(
            "SELECT type,name,tbl_name,sql FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%' ORDER BY type,name",
        )
        .map_err(storage)?;
    statement
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)
}

fn require_user(connection: &Connection, user_id: &str) -> Result<(), PrincipalBindingError> {
    let exists = connection
        .query_row(
            "SELECT 1 FROM users WHERE user_id=?1",
            [user_id],
            |_| Ok(()),
        )
        .optional()
        .map_err(storage)?
        .is_some();
    if exists {
        Ok(())
    } else {
        Err(PrincipalBindingError::UnknownUser(user_id.to_owned()))
    }
}

fn load_binding(
    connection: &Connection,
    transport: &str,
    instance: &str,
    external_digest: &str,
) -> Result<Option<PrincipalBinding>, PrincipalBindingError> {
    connection
        .query_row(
            "SELECT user_id,revision,linked_at FROM principal_bindings WHERE transport=?1 AND channel_instance_id=?2 AND external_principal_digest=?3 AND user_id IS NOT NULL",
            params![transport, instance, external_digest],
            |row| {
                let revision = row.get::<_, i64>(1)?;
                let revision = u64::try_from(revision).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        1,
                        rusqlite::types::Type::Integer,
                        Box::new(error),
                    )
                })?;
                Ok(PrincipalBinding {
                    transport: transport.to_owned(),
                    channel_instance_id: instance.to_owned(),
                    user_id: UserId::new(row.get::<_, String>(0)?),
                    revision,
                    linked_at: row.get(2)?,
                })
            },
        )
        .optional()
        .map_err(storage)
}

fn load_binding_revision(
    connection: &Connection,
    transport: &str,
    instance: &str,
    external_digest: &str,
) -> Result<Option<u64>, PrincipalBindingError> {
    connection
        .query_row(
            "SELECT revision FROM principal_bindings WHERE transport=?1 AND channel_instance_id=?2 AND external_principal_digest=?3",
            params![transport, instance, external_digest],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(storage)?
        .map(|revision| u64::try_from(revision).map_err(|_| PrincipalBindingError::Storage))
        .transpose()
}

fn load_challenge(
    connection: &Connection,
    challenge_id: &str,
) -> Result<Option<StoredChallenge>, PrincipalBindingError> {
    connection
        .query_row(
            "SELECT target_user_id,secret_hash,expires_at,attempts FROM link_challenges WHERE challenge_id=?1",
            [challenge_id],
            |row| {
                Ok(StoredChallenge {
                    target_user: row.get(0)?,
                    secret_hash: row.get(1)?,
                    expires_at: row.get(2)?,
                    attempts: row.get(3)?,
                })
            },
        )
        .optional()
        .map_err(storage)
}

fn challenge_digest(challenge_id: &str, secret: &str) -> String {
    hex_digest(&[
        b"sylvander.principal-link-challenge.v1",
        challenge_id.as_bytes(),
        secret.as_bytes(),
    ])
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0_u8;
    for (left, right) in left.iter().zip(right) {
        difference |= left ^ right;
    }
    difference == 0
}

fn is_unique_constraint(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(inner, _)
            if inner.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY
                || inner.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
    )
}

fn storage(_: rusqlite::Error) -> PrincipalBindingError {
    PrincipalBindingError::Storage
}

const SCHEMA: &str = r"
BEGIN IMMEDIATE;
PRAGMA application_id=1398362178;
CREATE TABLE users (
    user_id TEXT PRIMARY KEY NOT NULL
        CHECK(length(user_id) BETWEEN 1 AND 512 AND user_id=trim(user_id)),
    created_at INTEGER NOT NULL
) STRICT;
CREATE TABLE principal_binding_metadata (
    singleton INTEGER PRIMARY KEY NOT NULL CHECK(singleton=1),
    key_verifier TEXT NOT NULL CHECK(length(key_verifier)=64)
) STRICT;
CREATE TABLE principal_bindings (
    transport TEXT NOT NULL CHECK(length(transport) BETWEEN 1 AND 512),
    channel_instance_id TEXT NOT NULL CHECK(length(channel_instance_id) BETWEEN 1 AND 512),
    external_principal_digest TEXT NOT NULL CHECK(length(external_principal_digest)=64),
    user_id TEXT,
    revision INTEGER NOT NULL CHECK(revision > 0),
    linked_at INTEGER NOT NULL,
    unlinked_at INTEGER,
    PRIMARY KEY(transport,channel_instance_id,external_principal_digest),
    CHECK((user_id IS NULL)=(unlinked_at IS NOT NULL)),
    FOREIGN KEY(user_id) REFERENCES users(user_id) ON DELETE RESTRICT
) STRICT, WITHOUT ROWID;
CREATE INDEX principal_bindings_by_user ON principal_bindings(user_id);
CREATE TABLE link_challenges (
    challenge_id TEXT PRIMARY KEY NOT NULL,
    target_user_id TEXT NOT NULL UNIQUE,
    secret_hash TEXT NOT NULL CHECK(length(secret_hash)=64),
    expires_at INTEGER NOT NULL,
    attempts INTEGER NOT NULL CHECK(attempts BETWEEN 0 AND 4),
    created_at INTEGER NOT NULL,
    FOREIGN KEY(target_user_id) REFERENCES users(user_id) ON DELETE CASCADE
) STRICT;
PRAGMA user_version=1;
COMMIT;
";
