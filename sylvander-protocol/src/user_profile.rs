//! Public, owner-safe User Profile subprotocol.
//!
//! Requests never carry a `UserId`. Runtime derives the owner from its
//! authenticated boundary and applies every operation to that owner only.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

use crate::{UiProtocolHello, UiProtocolWelcome};

/// Current and only supported User Profile protocol version.
pub const USER_PROFILE_PROTOCOL_VERSION: u16 = 1;
pub const USER_PROFILE_CAPABILITY: &str = "user_profile_v1";
const MAX_CONSTRAINTS: usize = 16;

/// Runtime-supported versions. Empty means fail-closed denial.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UserProfileCapabilities {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub versions: Vec<u16>,
}

impl UserProfileCapabilities {
    #[must_use]
    pub fn current() -> Self {
        Self {
            versions: vec![USER_PROFILE_PROTOCOL_VERSION],
        }
    }

    #[must_use]
    pub fn supports(&self, version: u16) -> bool {
        self.versions.contains(&version)
    }
}

/// One operation against the boundary-derived owner profile.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UserProfileRequest {
    pub version: u16,
    pub action: UserProfileAction,
}

impl UserProfileRequest {
    pub fn validate(&self) -> Result<(), UserProfileValidationError> {
        if self.version != USER_PROFILE_PROTOCOL_VERSION {
            return Err(UserProfileValidationError::UnsupportedVersion);
        }
        self.action.validate()
    }

    #[must_use]
    pub const fn operation(&self) -> UserProfileOperation {
        self.action.operation()
    }
}

impl fmt::Debug for UserProfileRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UserProfileRequest")
            .field("version", &self.version)
            .field("operation", &self.operation())
            .field("profile_data", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

/// Versioned CRUD plus explicit privacy-right controls.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum UserProfileAction {
    Create {
        profile: UserProfileData,
    },
    Read {},
    Update {
        expected_revision: u64,
        profile: UserProfileData,
    },
    Export {
        format: UserProfileExportFormat,
    },
    Correct {
        expected_revision: u64,
        profile: UserProfileData,
    },
    Delete {
        expected_revision: u64,
    },
    SetDoNotLearn {
        expected_revision: u64,
        enabled: bool,
    },
}

impl UserProfileAction {
    fn validate(&self) -> Result<(), UserProfileValidationError> {
        match self {
            Self::Create { profile } => profile.validate(),
            Self::Update {
                expected_revision,
                profile,
            }
            | Self::Correct {
                expected_revision,
                profile,
            } => {
                validate_revision(*expected_revision)?;
                profile.validate()
            }
            Self::Delete { expected_revision }
            | Self::SetDoNotLearn {
                expected_revision, ..
            } => validate_revision(*expected_revision),
            Self::Read {} | Self::Export { .. } => Ok(()),
        }
    }

    #[must_use]
    pub const fn operation(&self) -> UserProfileOperation {
        match self {
            Self::Create { .. } => UserProfileOperation::Create,
            Self::Read {} => UserProfileOperation::Read,
            Self::Update { .. } => UserProfileOperation::Update,
            Self::Export { .. } => UserProfileOperation::Export,
            Self::Correct { .. } => UserProfileOperation::Correct,
            Self::Delete { .. } => UserProfileOperation::Delete,
            Self::SetDoNotLearn { .. } => UserProfileOperation::SetDoNotLearn,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UserProfileOperation {
    Create,
    Read,
    Update,
    Export,
    Correct,
    Delete,
    SetDoNotLearn,
}

/// Portable export encoding. Additional formats require a new protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UserProfileExportFormat {
    Json,
}

/// Privacy controls applied independently to every preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyClass {
    Personal,
    Sensitive,
    Restricted,
}

/// A typed preference with an explicit privacy classification.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClassifiedPreference<T> {
    pub value: T,
    pub privacy_class: PrivacyClass,
}

impl<T> fmt::Debug for ClassifiedPreference<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClassifiedPreference")
            .field("value", &"[REDACTED]")
            .field("privacy_class", &self.privacy_class)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ResponseDetail {
    Concise,
    Balanced,
    Detailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CommunicationTone {
    Direct,
    Warm,
    Formal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AccessibilityPreferences {
    #[serde(default)]
    pub screen_reader_optimized: bool,
    #[serde(default)]
    pub reduce_motion: bool,
    #[serde(default)]
    pub high_contrast: bool,
}

/// Complete replaceable profile payload. There is no untyped extension map.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UserProfileData {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_language: Option<ClassifiedPreference<LanguageTag>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locale: Option<ClassifiedPreference<LocaleId>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_detail: Option<ClassifiedPreference<ResponseDetail>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub communication_tone: Option<ClassifiedPreference<CommunicationTone>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accessibility: Option<ClassifiedPreference<AccessibilityPreferences>>,
    #[serde(default, deserialize_with = "deserialize_constraints")]
    pub constraints: Vec<ClassifiedPreference<ProfileConstraint>>,
}

impl UserProfileData {
    fn validate(&self) -> Result<(), UserProfileValidationError> {
        if self.constraints.len() > MAX_CONSTRAINTS {
            return Err(UserProfileValidationError::TooManyConstraints);
        }
        Ok(())
    }
}

impl fmt::Debug for UserProfileData {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UserProfileData")
            .field("preferences", &"[REDACTED]")
            .field("constraint_count", &self.constraints.len())
            .finish_non_exhaustive()
    }
}

/// Revisioned profile view for the boundary-derived owner.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UserProfileView {
    pub revision: u64,
    pub profile: UserProfileData,
    pub do_not_learn: bool,
    pub created_at_unix_secs: i64,
    pub updated_at_unix_secs: i64,
}

impl fmt::Debug for UserProfileView {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UserProfileView")
            .field("revision", &self.revision)
            .field("profile", &"[REDACTED]")
            .field("do_not_learn", &self.do_not_learn)
            .field("created_at_unix_secs", &self.created_at_unix_secs)
            .field("updated_at_unix_secs", &self.updated_at_unix_secs)
            .finish()
    }
}

/// Portable, self-describing export. The owner is deliberately absent.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UserProfileExport {
    pub schema_version: u16,
    pub format: UserProfileExportFormat,
    pub profile: UserProfileView,
    pub exported_at_unix_secs: i64,
}

impl fmt::Debug for UserProfileExport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UserProfileExport")
            .field("schema_version", &self.schema_version)
            .field("format", &self.format)
            .field("profile", &"[REDACTED]")
            .field("exported_at_unix_secs", &self.exported_at_unix_secs)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum UserProfileResponse {
    Created {
        version: u16,
        profile: UserProfileView,
    },
    Read {
        version: u16,
        profile: UserProfileView,
    },
    Updated {
        version: u16,
        profile: UserProfileView,
    },
    Exported {
        version: u16,
        export: UserProfileExport,
    },
    Corrected {
        version: u16,
        profile: UserProfileView,
    },
    Deleted {
        version: u16,
        deleted_revision: u64,
        do_not_learn_preserved: bool,
    },
    DoNotLearnUpdated {
        version: u16,
        profile: UserProfileView,
    },
    NotFound {
        version: u16,
    },
    Error {
        version: u16,
        error: UserProfileError,
    },
}

impl UserProfileResponse {
    const fn kind(&self) -> &'static str {
        match self {
            Self::Created { .. } => "created",
            Self::Read { .. } => "read",
            Self::Updated { .. } => "updated",
            Self::Exported { .. } => "exported",
            Self::Corrected { .. } => "corrected",
            Self::Deleted { .. } => "deleted",
            Self::DoNotLearnUpdated { .. } => "do_not_learn_updated",
            Self::NotFound { .. } => "not_found",
            Self::Error { .. } => "error",
        }
    }
}

impl fmt::Debug for UserProfileResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UserProfileResponse")
            .field("result", &self.kind())
            .field("profile_data", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

/// Stable, content-free public error. Storage/provider details never cross it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UserProfileError {
    pub code: UserProfileErrorCode,
    pub operation: UserProfileOperation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
}

impl UserProfileError {
    #[must_use]
    pub const fn service_unavailable(operation: UserProfileOperation) -> Self {
        Self {
            code: UserProfileErrorCode::ServiceUnavailable,
            operation,
            current_revision: None,
            retry_after_ms: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UserProfileErrorCode {
    UnsupportedVersion,
    InvalidRequest,
    Unauthenticated,
    Forbidden,
    NotFound,
    AlreadyExists,
    Conflict,
    RateLimited,
    ServiceUnavailable,
    Internal,
}

macro_rules! bounded_text {
    ($name:ident, $max:expr, $error:expr) => {
        #[derive(Clone, PartialEq, Eq, Serialize, JsonSchema)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, UserProfileValidationError> {
                let value = value.into();
                validate_text(&value, $max, $error)?;
                Ok(Self(value))
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(concat!(stringify!($name), "([REDACTED])"))
            }
        }
    };
}

bounded_text!(LanguageTag, 64, UserProfileValidationError::InvalidLanguage);
bounded_text!(LocaleId, 64, UserProfileValidationError::InvalidLocale);
bounded_text!(
    ProfileConstraint,
    512,
    UserProfileValidationError::InvalidConstraint
);

fn deserialize_constraints<'de, D>(
    deserializer: D,
) -> Result<Vec<ClassifiedPreference<ProfileConstraint>>, D::Error>
where
    D: Deserializer<'de>,
{
    let values = Vec::deserialize(deserializer)?;
    if values.len() > MAX_CONSTRAINTS {
        return Err(serde::de::Error::custom("too many profile constraints"));
    }
    Ok(values)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserProfileValidationError {
    UnsupportedVersion,
    InvalidRevision,
    InvalidLanguage,
    InvalidLocale,
    InvalidConstraint,
    TooManyConstraints,
}

impl fmt::Display for UserProfileValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::UnsupportedVersion => "unsupported user profile protocol version",
            Self::InvalidRevision => "invalid user profile revision",
            Self::InvalidLanguage => "invalid preferred language",
            Self::InvalidLocale => "invalid locale",
            Self::InvalidConstraint => "invalid profile constraint",
            Self::TooManyConstraints => "too many profile constraints",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for UserProfileValidationError {}

/// True only when both UI peers explicitly advertise User Profile v1.
#[must_use]
pub fn user_profile_is_negotiated(hello: &UiProtocolHello, welcome: &UiProtocolWelcome) -> bool {
    hello.min_version <= welcome.version
        && welcome.version <= hello.max_version
        && hello
            .capabilities
            .iter()
            .any(|candidate| candidate == USER_PROFILE_CAPABILITY)
        && welcome
            .capabilities
            .iter()
            .any(|candidate| candidate == USER_PROFILE_CAPABILITY)
}

fn validate_revision(revision: u64) -> Result<(), UserProfileValidationError> {
    if revision == 0 {
        return Err(UserProfileValidationError::InvalidRevision);
    }
    Ok(())
}

fn validate_text(
    value: &str,
    max_bytes: usize,
    error: UserProfileValidationError,
) -> Result<(), UserProfileValidationError> {
    if value.is_empty()
        || value.trim() != value
        || value.len() > max_bytes
        || value.chars().any(char::is_control)
    {
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
