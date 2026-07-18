//! Latest-only durable storage for Runtime-owned user profiles.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use sylvander_protocol::{
    USER_PROFILE_PROTOCOL_VERSION, UserId, UserProfileData, UserProfileExport,
    UserProfileExportFormat, UserProfileView,
};
use tokio::{sync::Mutex, task};

use sylvander_agent::user_profile_provider::{
    UserProfileProvider, UserProfileProviderError, UserProfileSubject,
};

const APPLICATION_ID: i64 = 1_398_362_182;
const SCHEMA_VERSION: i64 = 1;
const MAX_USER_ID_BYTES: usize = 512;
const MAX_PROFILE_JSON_BYTES: usize = 16 * 1024;
const MAX_CONSTRAINTS: usize = 16;

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct StoredUserProfile {
    pub(crate) owner: UserId,
    pub(crate) revision: u64,
    pub(crate) profile: UserProfileData,
    pub(crate) do_not_learn: bool,
    pub(crate) created_at_unix_secs: i64,
    pub(crate) updated_at_unix_secs: i64,
}

impl std::fmt::Debug for StoredUserProfile {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StoredUserProfile")
            .field("owner", &self.owner)
            .field("revision", &self.revision)
            .field("profile", &"[REDACTED]")
            .field("do_not_learn", &self.do_not_learn)
            .field("created_at_unix_secs", &self.created_at_unix_secs)
            .field("updated_at_unix_secs", &self.updated_at_unix_secs)
            .finish()
    }
}

impl StoredUserProfile {
    pub(crate) fn into_view(self) -> UserProfileView {
        UserProfileView {
            revision: self.revision,
            profile: self.profile,
            do_not_learn: self.do_not_learn,
            created_at_unix_secs: self.created_at_unix_secs,
            updated_at_unix_secs: self.updated_at_unix_secs,
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum UserProfileStoreError {
    #[error("invalid user profile {0}")]
    Invalid(&'static str),
    #[error("user profile already exists")]
    AlreadyExists,
    #[error("user profile does not exist")]
    NotFound,
    #[error("user profile revision conflict: expected {expected}, found {actual}")]
    Conflict { expected: u64, actual: u64 },
    #[error("user profile database schema is incompatible")]
    IncompatibleSchema,
    #[error("user profile data is corrupt")]
    Corrupt,
    #[error("user profile storage failed")]
    Storage,
    #[error("user profile storage task failed")]
    Task,
}

#[derive(Clone)]
pub(crate) struct UserProfileStore {
    connection: Arc<Mutex<Connection>>,
    clock: Arc<dyn Clock>,
}

trait Clock: Send + Sync {
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

impl UserProfileStore {
    pub(crate) async fn open(path: impl AsRef<Path>) -> Result<Self, UserProfileStoreError> {
        let path = path.as_ref().to_path_buf();
        Self::open_with(move || Connection::open(path), Arc::new(SystemClock)).await
    }

    #[cfg(test)]
    async fn open_in_memory(clock: Arc<dyn Clock>) -> Result<Self, UserProfileStoreError> {
        Self::open_with(Connection::open_in_memory, clock).await
    }

    async fn open_with(
        open: impl FnOnce() -> rusqlite::Result<Connection> + Send + 'static,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, UserProfileStoreError> {
        let connection = task::spawn_blocking(move || {
            let mut connection = open().map_err(storage)?;
            connection
                .busy_timeout(Duration::from_secs(5))
                .map_err(storage)?;
            initialize_schema(&mut connection)?;
            Ok(connection)
        })
        .await
        .map_err(|_| UserProfileStoreError::Task)??;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            clock,
        })
    }

    async fn run<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Connection) -> Result<T, UserProfileStoreError> + Send + 'static,
    ) -> Result<T, UserProfileStoreError> {
        let connection = self.connection.clone();
        task::spawn_blocking(move || {
            let mut connection = connection.blocking_lock();
            operation(&mut connection)
        })
        .await
        .map_err(|_| UserProfileStoreError::Task)?
    }

    pub(crate) async fn read(
        &self,
        owner: UserId,
    ) -> Result<Option<StoredUserProfile>, UserProfileStoreError> {
        validate_owner(&owner)?;
        self.run(move |connection| load_profile(connection, &owner))
            .await
    }

    pub(crate) async fn create(
        &self,
        owner: UserId,
        profile: UserProfileData,
    ) -> Result<StoredUserProfile, UserProfileStoreError> {
        validate_owner(&owner)?;
        let profile_json = encode_profile(&profile)?;
        let now = self.clock.now();
        self.run(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(storage)?;
            let previous = load_head(&transaction, &owner)?;
            if previous.as_ref().is_some_and(|head| !head.deleted) {
                return Err(UserProfileStoreError::AlreadyExists);
            }
            let previous_revision = previous.map_or(0, |head| head.revision);
            let revision = next_revision(previous_revision)?;
            let changed = transaction
                .execute(
                    "INSERT INTO user_profiles(user_id,revision,profile_json,do_not_learn,deleted,created_at,updated_at) VALUES (?1,?2,?3,0,0,?4,?4) ON CONFLICT(user_id) DO UPDATE SET revision=excluded.revision,profile_json=excluded.profile_json,do_not_learn=user_profiles.do_not_learn,deleted=0,created_at=excluded.created_at,updated_at=excluded.updated_at WHERE user_profiles.deleted=1 AND user_profiles.revision=?5",
                    params![owner.0, sql_revision(revision)?, profile_json, now, sql_revision(previous_revision)?],
                )
                .map_err(storage)?;
            if changed != 1 {
                return Err(UserProfileStoreError::AlreadyExists);
            }
            let stored = load_profile(&transaction, &owner)?.ok_or(UserProfileStoreError::Storage)?;
            transaction.commit().map_err(storage)?;
            Ok(stored)
        })
        .await
    }

    pub(crate) async fn update(
        &self,
        owner: UserId,
        expected_revision: u64,
        profile: UserProfileData,
    ) -> Result<StoredUserProfile, UserProfileStoreError> {
        self.replace(owner, expected_revision, profile).await
    }

    pub(crate) async fn correct(
        &self,
        owner: UserId,
        expected_revision: u64,
        profile: UserProfileData,
    ) -> Result<StoredUserProfile, UserProfileStoreError> {
        self.replace(owner, expected_revision, profile).await
    }

    async fn replace(
        &self,
        owner: UserId,
        expected_revision: u64,
        profile: UserProfileData,
    ) -> Result<StoredUserProfile, UserProfileStoreError> {
        let profile_json = encode_profile(&profile)?;
        let now = self.clock.now();
        self.mutate(owner, expected_revision, move |connection, owner, revision| {
            connection
                .execute(
                    "UPDATE user_profiles SET revision=?3,profile_json=?4,updated_at=?5 WHERE user_id=?1 AND revision=?2 AND deleted=0",
                    params![owner.0, sql_revision(expected_revision)?, sql_revision(revision)?, profile_json, now],
                )
                .map_err(storage)
        })
        .await
    }

    pub(crate) async fn set_do_not_learn(
        &self,
        owner: UserId,
        expected_revision: u64,
        enabled: bool,
    ) -> Result<StoredUserProfile, UserProfileStoreError> {
        let now = self.clock.now();
        self.mutate(owner, expected_revision, move |connection, owner, revision| {
            connection
                .execute(
                    "UPDATE user_profiles SET revision=?3,do_not_learn=?4,updated_at=?5 WHERE user_id=?1 AND revision=?2 AND deleted=0",
                    params![owner.0, sql_revision(expected_revision)?, sql_revision(revision)?, enabled, now],
                )
                .map_err(storage)
        })
        .await
    }

    pub(crate) async fn delete(
        &self,
        owner: UserId,
        expected_revision: u64,
    ) -> Result<u64, UserProfileStoreError> {
        validate_owner(&owner)?;
        validate_expected_revision(expected_revision)?;
        let now = self.clock.now();
        let revision = next_revision(expected_revision)?;
        self.run(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(storage)?;
            require_revision(&transaction, &owner, expected_revision)?;
            transaction
                .execute(
                    "UPDATE user_profiles SET revision=?3,profile_json=NULL,do_not_learn=1,deleted=1,updated_at=?4 WHERE user_id=?1 AND revision=?2 AND deleted=0",
                    params![owner.0, sql_revision(expected_revision)?, sql_revision(revision)?, now],
                )
                .map_err(storage)?;
            transaction.commit().map_err(storage)?;
            Ok(revision)
        })
        .await
    }

    pub(crate) async fn export(
        &self,
        owner: UserId,
    ) -> Result<UserProfileExport, UserProfileStoreError> {
        let stored = self
            .read(owner)
            .await?
            .ok_or(UserProfileStoreError::NotFound)?;
        Ok(UserProfileExport {
            schema_version: USER_PROFILE_PROTOCOL_VERSION,
            format: UserProfileExportFormat::Json,
            profile: stored.into_view(),
            exported_at_unix_secs: self.clock.now(),
        })
    }

    async fn mutate(
        &self,
        owner: UserId,
        expected_revision: u64,
        mutation: impl FnOnce(&Connection, &UserId, u64) -> Result<usize, UserProfileStoreError>
        + Send
        + 'static,
    ) -> Result<StoredUserProfile, UserProfileStoreError> {
        validate_owner(&owner)?;
        validate_expected_revision(expected_revision)?;
        self.run(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(storage)?;
            require_revision(&transaction, &owner, expected_revision)?;
            let revision = next_revision(expected_revision)?;
            if mutation(&transaction, &owner, revision)? != 1 {
                return Err(UserProfileStoreError::Storage);
            }
            let stored =
                load_profile(&transaction, &owner)?.ok_or(UserProfileStoreError::Storage)?;
            transaction.commit().map_err(storage)?;
            Ok(stored)
        })
        .await
    }
}

#[async_trait::async_trait]
impl UserProfileProvider for UserProfileStore {
    async fn current_profile(
        &self,
        subject: &UserProfileSubject,
    ) -> Result<Option<UserProfileView>, UserProfileProviderError> {
        self.read(subject.user_id().clone())
            .await
            .map(|profile| profile.map(StoredUserProfile::into_view))
            .map_err(|_| UserProfileProviderError::Unavailable)
    }
}

struct ProfileHead {
    revision: u64,
    deleted: bool,
}

fn load_head(
    connection: &Connection,
    owner: &UserId,
) -> Result<Option<ProfileHead>, UserProfileStoreError> {
    connection
        .query_row(
            "SELECT revision,deleted FROM user_profiles WHERE user_id=?1",
            [&owner.0],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, bool>(1)?)),
        )
        .optional()
        .map_err(storage)?
        .map(|(revision, deleted)| {
            Ok(ProfileHead {
                revision: u64::try_from(revision).map_err(|_| UserProfileStoreError::Corrupt)?,
                deleted,
            })
        })
        .transpose()
}

fn require_revision(
    connection: &Connection,
    owner: &UserId,
    expected: u64,
) -> Result<(), UserProfileStoreError> {
    let head = load_head(connection, owner)?.ok_or(UserProfileStoreError::NotFound)?;
    if head.deleted {
        return Err(UserProfileStoreError::NotFound);
    }
    if head.revision != expected {
        return Err(UserProfileStoreError::Conflict {
            expected,
            actual: head.revision,
        });
    }
    Ok(())
}

fn validate_expected_revision(revision: u64) -> Result<(), UserProfileStoreError> {
    if revision == 0 {
        Err(UserProfileStoreError::Invalid("revision"))
    } else {
        Ok(())
    }
}

fn next_revision(revision: u64) -> Result<u64, UserProfileStoreError> {
    revision
        .checked_add(1)
        .ok_or(UserProfileStoreError::Storage)
}

fn sql_revision(revision: u64) -> Result<i64, UserProfileStoreError> {
    i64::try_from(revision).map_err(|_| UserProfileStoreError::Invalid("revision"))
}

fn validate_owner(owner: &UserId) -> Result<(), UserProfileStoreError> {
    if owner == &UserId::system()
        || owner.0.is_empty()
        || owner.0.trim() != owner.0
        || owner.0.len() > MAX_USER_ID_BYTES
        || owner.0.chars().any(char::is_control)
    {
        return Err(UserProfileStoreError::Invalid("owner"));
    }
    Ok(())
}

fn encode_profile(profile: &UserProfileData) -> Result<String, UserProfileStoreError> {
    if profile.constraints.len() > MAX_CONSTRAINTS {
        return Err(UserProfileStoreError::Invalid("constraints"));
    }
    let encoded =
        serde_json::to_string(profile).map_err(|_| UserProfileStoreError::Invalid("payload"))?;
    if encoded.len() > MAX_PROFILE_JSON_BYTES {
        return Err(UserProfileStoreError::Invalid("payload"));
    }
    Ok(encoded)
}

fn decode_profile(encoded: &str) -> Result<UserProfileData, UserProfileStoreError> {
    if encoded.len() > MAX_PROFILE_JSON_BYTES {
        return Err(UserProfileStoreError::Corrupt);
    }
    let profile: UserProfileData =
        serde_json::from_str(encoded).map_err(|_| UserProfileStoreError::Corrupt)?;
    if encode_profile(&profile).map_err(|_| UserProfileStoreError::Corrupt)? != encoded {
        return Err(UserProfileStoreError::Corrupt);
    }
    Ok(profile)
}

fn load_profile(
    connection: &Connection,
    owner: &UserId,
) -> Result<Option<StoredUserProfile>, UserProfileStoreError> {
    connection
        .query_row(
            "SELECT revision,profile_json,do_not_learn,created_at,updated_at FROM user_profiles WHERE user_id=?1 AND deleted=0",
            [&owner.0],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, bool>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            },
        )
        .optional()
        .map_err(storage)?
        .map(|(revision, profile, do_not_learn, created_at, updated_at)| {
            Ok(StoredUserProfile {
                owner: owner.clone(),
                revision: u64::try_from(revision).map_err(|_| UserProfileStoreError::Corrupt)?,
                profile: decode_profile(&profile)?,
                do_not_learn,
                created_at_unix_secs: created_at,
                updated_at_unix_secs: updated_at,
            })
        })
        .transpose()
}

fn initialize_schema(connection: &mut Connection) -> Result<(), UserProfileStoreError> {
    let version = connection
        .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
        .map_err(storage)?;
    let application_id = connection
        .query_row("PRAGMA application_id", [], |row| row.get::<_, i64>(0))
        .map_err(storage)?;
    let object_count = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map_err(storage)?;
    match (version, application_id, object_count) {
        (0, 0, 0) => connection.execute_batch(SCHEMA).map_err(storage),
        (SCHEMA_VERSION, APPLICATION_ID, _) => validate_schema(connection),
        _ => Err(UserProfileStoreError::IncompatibleSchema),
    }?;
    let integrity = connection
        .query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0))
        .map_err(storage)?;
    if integrity == "ok" {
        Ok(())
    } else {
        Err(UserProfileStoreError::Corrupt)
    }
}

fn validate_schema(connection: &Connection) -> Result<(), UserProfileStoreError> {
    let expected = Connection::open_in_memory().map_err(storage)?;
    expected.execute_batch(SCHEMA).map_err(storage)?;
    if schema_objects(connection)? == schema_objects(&expected)? {
        Ok(())
    } else {
        Err(UserProfileStoreError::IncompatibleSchema)
    }
}

fn schema_objects(
    connection: &Connection,
) -> Result<Vec<(String, String, String, String)>, UserProfileStoreError> {
    let mut statement = connection
        .prepare("SELECT type,name,tbl_name,sql FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%' ORDER BY type,name")
        .map_err(storage)?;
    statement
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)
}

fn storage(_: rusqlite::Error) -> UserProfileStoreError {
    UserProfileStoreError::Storage
}

const SCHEMA: &str = r"
BEGIN IMMEDIATE;
PRAGMA application_id=1398362182;
CREATE TABLE user_profiles (
    user_id TEXT PRIMARY KEY NOT NULL
        CHECK(length(user_id) BETWEEN 1 AND 512 AND user_id=trim(user_id)),
    revision INTEGER NOT NULL CHECK(revision > 0),
    profile_json TEXT CHECK(profile_json IS NULL OR length(profile_json) <= 16384),
    do_not_learn INTEGER NOT NULL CHECK(do_not_learn IN (0,1)),
    deleted INTEGER NOT NULL CHECK(deleted IN (0,1)),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    CHECK((deleted=1 AND profile_json IS NULL AND do_not_learn=1)
       OR (deleted=0 AND profile_json IS NOT NULL))
) STRICT, WITHOUT ROWID;
PRAGMA user_version=1;
COMMIT;
";

#[cfg(test)]
#[path = "../tests/unit/user_profile_store.rs"]
mod tests;
