//! Public identity-binding subprotocol.
//!
//! Requests describe only the action a caller wants to perform. They never
//! carry a transport, channel instance, or external principal. The Channel
//! layer derives that identity from authenticated ingress and supplies it to
//! the runtime through a separate, non-serializable envelope.
//!
//! The current protocol is latest-only. Unknown fields, unsupported versions,
//! invalid identifiers, and malformed secrets fail closed.

use std::borrow::Cow;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};

use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{UiProtocolHello, UiProtocolWelcome, UserId};

/// Current and only supported identity-binding subprotocol version.
pub const IDENTITY_BINDING_PROTOCOL_VERSION: u16 = 1;

/// Capability that both peers must advertise before identity operations.
pub const IDENTITY_BINDING_CAPABILITY: &str = "identity_binding_v1";

const MAX_CHALLENGE_ID_BYTES: usize = 512;
const MIN_SECRET_BYTES: usize = 16;
const MAX_SECRET_BYTES: usize = 512;

/// Runtime service versions available to a trusted Channel adapter.
///
/// Empty is the default and means every identity operation is denied.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IdentityBindingCapabilities {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub versions: Vec<u16>,
}

impl IdentityBindingCapabilities {
    #[must_use]
    pub fn current() -> Self {
        Self {
            versions: vec![IDENTITY_BINDING_PROTOCOL_VERSION],
        }
    }

    #[must_use]
    pub fn supports(&self, version: u16) -> bool {
        self.versions.contains(&version)
    }
}

/// One versioned identity-binding request.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IdentityBindingRequest {
    pub version: u16,
    pub action: IdentityBindingAction,
}

impl IdentityBindingRequest {
    /// Validate the exact version and every caller-controlled field.
    pub fn validate(&self) -> Result<(), IdentityBindingValidationError> {
        if self.version != IDENTITY_BINDING_PROTOCOL_VERSION {
            return Err(IdentityBindingValidationError::UnsupportedVersion);
        }
        self.action.validate()
    }

    #[must_use]
    pub const fn operation(&self) -> IdentityBindingOperation {
        self.action.operation()
    }
}

/// Action applied to the authenticated ingress principal.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum IdentityBindingAction {
    Begin {},
    Confirm {
        challenge_id: IdentityLinkChallengeId,
        proof: IdentityLinkSecretProof,
    },
    Resolve {},
    Unlink {
        expected_revision: u64,
    },
}

impl IdentityBindingAction {
    fn validate(&self) -> Result<(), IdentityBindingValidationError> {
        match self {
            Self::Confirm {
                challenge_id,
                proof,
            } => {
                challenge_id.validate()?;
                proof.validate()
            }
            Self::Begin {} | Self::Resolve {} | Self::Unlink { .. } => Ok(()),
        }
    }

    #[must_use]
    pub const fn operation(&self) -> IdentityBindingOperation {
        match self {
            Self::Begin {} => IdentityBindingOperation::Begin,
            Self::Confirm { .. } => IdentityBindingOperation::Confirm,
            Self::Resolve {} => IdentityBindingOperation::Resolve,
            Self::Unlink { .. } => IdentityBindingOperation::Unlink,
        }
    }
}

/// Stable operation name used by policy, audit, and public errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IdentityBindingOperation {
    Begin,
    Confirm,
    Resolve,
    Unlink,
}

/// Opaque challenge identity. It is not an external principal identifier.
#[derive(Debug, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct IdentityLinkChallengeId(String);

impl IdentityLinkChallengeId {
    pub fn new(value: impl Into<String>) -> Result<Self, IdentityBindingValidationError> {
        let value = value.into();
        validate_text(
            &value,
            MAX_CHALLENGE_ID_BYTES,
            IdentityBindingValidationError::InvalidChallengeId,
        )?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn validate(&self) -> Result<(), IdentityBindingValidationError> {
        validate_text(
            &self.0,
            MAX_CHALLENGE_ID_BYTES,
            IdentityBindingValidationError::InvalidChallengeId,
        )
    }
}

impl<'de> Deserialize<'de> for IdentityLinkChallengeId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// Secret proof returned to the runtime during confirmation.
///
/// `Debug` is deliberately redacted. The value is serializable only because a
/// client must return it in the subsequent confirmation request.
#[derive(PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct IdentityLinkSecretProof(String);

impl IdentityLinkSecretProof {
    pub fn new(value: impl Into<String>) -> Result<Self, IdentityBindingValidationError> {
        let value = value.into();
        validate_secret(&value)?;
        Ok(Self(value))
    }

    /// Expose the proof only at the runtime verification boundary.
    #[must_use]
    pub fn expose_for_verification(&self) -> &str {
        &self.0
    }

    fn validate(&self) -> Result<(), IdentityBindingValidationError> {
        validate_secret(&self.0)
    }
}

impl<'de> Deserialize<'de> for IdentityLinkSecretProof {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

impl fmt::Debug for IdentityLinkSecretProof {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("IdentityLinkSecretProof([REDACTED])")
    }
}

/// One-time secret emitted only by [`IdentityBindingResponse::ChallengeIssued`].
///
/// It is intentionally not `Clone`, has no display implementation, and always
/// redacts `Debug`. A client consumes it into a confirmation proof.
pub struct OneTimeIdentityLinkSecret {
    value: String,
    serialized: AtomicBool,
}

impl OneTimeIdentityLinkSecret {
    pub fn new(value: impl Into<String>) -> Result<Self, IdentityBindingValidationError> {
        let value = value.into();
        validate_secret(&value)?;
        Ok(Self {
            value,
            serialized: AtomicBool::new(false),
        })
    }

    #[must_use]
    pub fn into_confirmation_proof(self) -> IdentityLinkSecretProof {
        IdentityLinkSecretProof(self.value)
    }
}

impl PartialEq for OneTimeIdentityLinkSecret {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}

impl Eq for OneTimeIdentityLinkSecret {}

impl fmt::Debug for OneTimeIdentityLinkSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OneTimeIdentityLinkSecret([REDACTED])")
    }
}

impl Serialize for OneTimeIdentityLinkSecret {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if self.serialized.swap(true, Ordering::AcqRel) {
            return Err(serde::ser::Error::custom(
                "one-time identity link secret was already serialized",
            ));
        }
        serializer.serialize_str(&self.value)
    }
}

impl<'de> Deserialize<'de> for OneTimeIdentityLinkSecret {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for OneTimeIdentityLinkSecret {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("OneTimeIdentityLinkSecret")
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        generator.subschema_for::<String>()
    }
}

/// Principal binding view safe to return to the currently authenticated peer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IdentityBindingView {
    pub user_id: UserId,
    pub revision: u64,
    pub linked_at_unix_secs: i64,
}

/// Typed identity operation response.
///
/// No variant other than `ChallengeIssued` can contain a link secret.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum IdentityBindingResponse {
    ChallengeIssued {
        version: u16,
        challenge_id: IdentityLinkChallengeId,
        secret: OneTimeIdentityLinkSecret,
        expires_at_unix_secs: i64,
    },
    Resolved {
        version: u16,
        binding: IdentityBindingView,
    },
    NotLinked {
        version: u16,
    },
    Unlinked {
        version: u16,
    },
    Error {
        version: u16,
        error: IdentityBindingError,
    },
}

/// Content-safe public identity-binding failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IdentityBindingError {
    pub code: IdentityBindingErrorCode,
    pub operation: IdentityBindingOperation,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
}

impl IdentityBindingError {
    #[must_use]
    pub fn service_unavailable(operation: IdentityBindingOperation) -> Self {
        Self {
            code: IdentityBindingErrorCode::ServiceUnavailable,
            operation,
            message: "identity binding service is unavailable".into(),
            retry_after_ms: None,
        }
    }
}

/// Stable public failure category. Provider and storage errors are never sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IdentityBindingErrorCode {
    UnsupportedVersion,
    InvalidRequest,
    Unauthenticated,
    Forbidden,
    AlreadyLinked,
    NotLinked,
    ChallengeUnavailable,
    ChallengeExpired,
    ChallengeRejected,
    Conflict,
    RateLimited,
    ServiceUnavailable,
    Internal,
}

/// Content-free local validation error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityBindingValidationError {
    UnsupportedVersion,
    InvalidChallengeId,
    InvalidSecret,
}

impl fmt::Display for IdentityBindingValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::UnsupportedVersion => "unsupported identity binding protocol version",
            Self::InvalidChallengeId => "invalid identity link challenge",
            Self::InvalidSecret => "invalid identity link secret",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for IdentityBindingValidationError {}

/// Return true only when both peers explicitly negotiated identity binding.
#[must_use]
pub fn identity_binding_is_negotiated(
    hello: &UiProtocolHello,
    welcome: &UiProtocolWelcome,
) -> bool {
    hello.min_version <= welcome.version
        && welcome.version <= hello.max_version
        && hello
            .capabilities
            .iter()
            .any(|candidate| candidate == IDENTITY_BINDING_CAPABILITY)
        && welcome
            .capabilities
            .iter()
            .any(|candidate| candidate == IDENTITY_BINDING_CAPABILITY)
}

fn validate_text(
    value: &str,
    max_bytes: usize,
    error: IdentityBindingValidationError,
) -> Result<(), IdentityBindingValidationError> {
    if value.is_empty()
        || value.trim() != value
        || value.len() > max_bytes
        || value.chars().any(char::is_control)
    {
        return Err(error);
    }
    Ok(())
}

fn validate_secret(value: &str) -> Result<(), IdentityBindingValidationError> {
    if !(MIN_SECRET_BYTES..=MAX_SECRET_BYTES).contains(&value.len())
        || value
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err(IdentityBindingValidationError::InvalidSecret);
    }
    Ok(())
}

#[cfg(test)]
#[path = "../tests/unit/identity_binding.rs"]
mod tests;
