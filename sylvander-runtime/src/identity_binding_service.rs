//! Runtime policy and protocol adapter for stable identity binding.

use std::collections::HashMap;
use std::time::Duration;

use sylvander_protocol::{
    BoundaryContext, IDENTITY_BINDING_PROTOCOL_VERSION, IdentityBindingAction,
    IdentityBindingError, IdentityBindingErrorCode, IdentityBindingOperation,
    IdentityBindingRequest, IdentityBindingResponse, IdentityBindingView, IdentityLinkChallengeId,
    OneTimeIdentityLinkSecret, PrincipalKind, UserId,
};

use crate::principal_binding::{
    ExternalPrincipal, PrincipalBinding, PrincipalBindingError, PrincipalBindingStore,
};

#[derive(Clone, PartialEq, Eq, Hash)]
struct IssuerKey {
    transport: String,
    channel_instance_id: String,
    principal_id: String,
}

/// Trusted stable-user identity vouched for by one exact authenticated ingress.
///
/// Instances come from validated server configuration, never request payloads.
pub(crate) struct TrustedIdentityIssuer {
    key: IssuerKey,
    user_id: UserId,
}

impl TrustedIdentityIssuer {
    pub(crate) fn new(
        transport: String,
        channel_instance_id: String,
        principal_id: String,
        user_id: UserId,
    ) -> Self {
        Self {
            key: IssuerKey {
                transport,
                channel_instance_id,
                principal_id,
            },
            user_id,
        }
    }
}

/// Identity tuple consumed from Channel's non-serializable ingress envelope.
pub(crate) struct IdentityIngress {
    key: IssuerKey,
}

impl IdentityIngress {
    pub(crate) fn new(
        transport: String,
        channel_instance_id: String,
        principal_id: String,
    ) -> Self {
        Self {
            key: IssuerKey {
                transport,
                channel_instance_id,
                principal_id,
            },
        }
    }

    fn matches_boundary(&self, boundary: &BoundaryContext) -> bool {
        boundary.principal.as_ref().is_some_and(|principal| {
            principal.kind == PrincipalKind::User
                && principal.id.0 == self.key.principal_id
                && boundary.transport == self.key.transport
                && boundary.channel_instance_id == self.key.channel_instance_id
        })
    }

    fn into_external_principal(self) -> Result<ExternalPrincipal, PrincipalBindingError> {
        ExternalPrincipal::new(
            self.key.transport,
            self.key.channel_instance_id,
            self.key.principal_id,
        )
    }
}

/// Runtime-owned identity service. Channels never receive the store or digest key.
pub(crate) struct IdentityBindingService {
    store: PrincipalBindingStore,
    issuers: HashMap<IssuerKey, UserId>,
    challenge_ttl: Duration,
}

impl IdentityBindingService {
    pub(crate) fn new(
        store: PrincipalBindingStore,
        issuers: impl IntoIterator<Item = TrustedIdentityIssuer>,
        challenge_ttl: Duration,
    ) -> Result<Self, PrincipalBindingError> {
        let mut by_ingress = HashMap::new();
        for issuer in issuers {
            if by_ingress.insert(issuer.key, issuer.user_id).is_some() {
                return Err(PrincipalBindingError::Invalid {
                    field: "identity issuer",
                    reason: "duplicate authenticated ingress".into(),
                });
            }
        }
        Ok(Self {
            store,
            issuers: by_ingress,
            challenge_ttl,
        })
    }

    pub(crate) async fn dispatch(
        &self,
        boundary: &BoundaryContext,
        ingress: IdentityIngress,
        request: IdentityBindingRequest,
    ) -> IdentityBindingResponse {
        let operation = request.operation();
        if request.validate().is_err() {
            return error_response(
                operation,
                IdentityBindingErrorCode::InvalidRequest,
                "identity binding request is invalid",
            );
        }
        if !ingress.matches_boundary(boundary) {
            return error_response(
                operation,
                IdentityBindingErrorCode::Forbidden,
                "the authenticated principal cannot bind an identity",
            );
        }

        match request.action {
            IdentityBindingAction::Begin {} => self.begin(&ingress.key).await,
            IdentityBindingAction::Confirm {
                challenge_id,
                proof,
            } => {
                let principal = match ingress.into_external_principal() {
                    Ok(principal) => principal,
                    Err(error) => return map_error(operation, error),
                };
                match self
                    .store
                    .confirm_link(
                        principal,
                        challenge_id.as_str(),
                        proof.expose_for_verification(),
                    )
                    .await
                {
                    Ok(binding) => resolved(binding),
                    Err(error) => map_error(operation, error),
                }
            }
            IdentityBindingAction::Resolve {} => {
                let principal = match ingress.into_external_principal() {
                    Ok(principal) => principal,
                    Err(error) => return map_error(operation, error),
                };
                match self.store.resolve(principal).await {
                    Ok(Some(binding)) => resolved(binding),
                    Ok(None) => IdentityBindingResponse::NotLinked {
                        version: IDENTITY_BINDING_PROTOCOL_VERSION,
                    },
                    Err(error) => map_error(operation, error),
                }
            }
            IdentityBindingAction::Unlink { expected_revision } => {
                self.unlink(ingress, expected_revision).await
            }
        }
    }

    /// Resolve one sealed ingress to the Runtime-owned user identity used by
    /// sessions, memory, and authorization.
    pub(crate) async fn resolve_user(
        &self,
        boundary: &BoundaryContext,
        ingress: IdentityIngress,
    ) -> Result<UserId, PrincipalBindingError> {
        if !ingress.matches_boundary(boundary) {
            return Err(PrincipalBindingError::Invalid {
                field: "identity ingress",
                reason: "does not match the authenticated boundary".into(),
            });
        }
        if let Some(user_id) = self.issuers.get(&ingress.key) {
            return Ok(user_id.clone());
        }
        let principal = ingress.into_external_principal()?;
        Ok(self.store.resolve(principal.clone()).await?.map_or_else(
            || self.store.isolated_user_id(&principal),
            |binding| binding.user_id,
        ))
    }

    async fn begin(&self, issuer: &IssuerKey) -> IdentityBindingResponse {
        let Some(user_id) = self.issuers.get(issuer).cloned() else {
            return error_response(
                IdentityBindingOperation::Begin,
                IdentityBindingErrorCode::Forbidden,
                "the authenticated principal cannot issue a link challenge",
            );
        };
        match self.store.begin_link(user_id, self.challenge_ttl).await {
            Ok(challenge) => {
                let Ok(challenge_id) = IdentityLinkChallengeId::new(challenge.challenge_id) else {
                    return error_response(
                        IdentityBindingOperation::Begin,
                        IdentityBindingErrorCode::Internal,
                        "identity binding operation failed",
                    );
                };
                let Ok(secret) = OneTimeIdentityLinkSecret::new(challenge.secret.expose()) else {
                    return error_response(
                        IdentityBindingOperation::Begin,
                        IdentityBindingErrorCode::Internal,
                        "identity binding operation failed",
                    );
                };
                IdentityBindingResponse::ChallengeIssued {
                    version: IDENTITY_BINDING_PROTOCOL_VERSION,
                    challenge_id,
                    secret,
                    expires_at_unix_secs: challenge.expires_at,
                }
            }
            Err(error) => map_error(IdentityBindingOperation::Begin, error),
        }
    }

    async fn unlink(
        &self,
        ingress: IdentityIngress,
        expected_revision: u64,
    ) -> IdentityBindingResponse {
        let principal = match ingress.into_external_principal() {
            Ok(principal) => principal,
            Err(error) => return map_error(IdentityBindingOperation::Unlink, error),
        };
        let binding = match self.store.resolve(principal.clone()).await {
            Ok(Some(binding)) => binding,
            Ok(None) => {
                return error_response(
                    IdentityBindingOperation::Unlink,
                    IdentityBindingErrorCode::NotLinked,
                    "the external principal is not linked",
                );
            }
            Err(error) => return map_error(IdentityBindingOperation::Unlink, error),
        };
        match self
            .store
            .unlink(principal, &binding.user_id, expected_revision)
            .await
        {
            Ok(()) => IdentityBindingResponse::Unlinked {
                version: IDENTITY_BINDING_PROTOCOL_VERSION,
            },
            Err(error) => map_error(IdentityBindingOperation::Unlink, error),
        }
    }
}

fn resolved(binding: PrincipalBinding) -> IdentityBindingResponse {
    IdentityBindingResponse::Resolved {
        version: IDENTITY_BINDING_PROTOCOL_VERSION,
        binding: IdentityBindingView {
            user_id: binding.user_id,
            revision: binding.revision,
            linked_at_unix_secs: binding.linked_at,
        },
    }
}

fn map_error(
    operation: IdentityBindingOperation,
    error: PrincipalBindingError,
) -> IdentityBindingResponse {
    let (code, message) = match error {
        PrincipalBindingError::AlreadyLinked => (
            IdentityBindingErrorCode::AlreadyLinked,
            "the external principal is already linked",
        ),
        PrincipalBindingError::UnknownBinding => (
            IdentityBindingErrorCode::NotLinked,
            "the external principal is not linked",
        ),
        PrincipalBindingError::UnknownChallenge => (
            IdentityBindingErrorCode::ChallengeUnavailable,
            "the identity link challenge is unavailable",
        ),
        PrincipalBindingError::ChallengeExpired => (
            IdentityBindingErrorCode::ChallengeExpired,
            "the identity link challenge expired",
        ),
        PrincipalBindingError::InvalidChallengeSecret | PrincipalBindingError::ChallengeLocked => (
            IdentityBindingErrorCode::ChallengeRejected,
            "the identity link challenge was rejected",
        ),
        PrincipalBindingError::Conflict { .. } => (
            IdentityBindingErrorCode::Conflict,
            "the identity binding revision changed",
        ),
        PrincipalBindingError::UnknownUser(_) => (
            IdentityBindingErrorCode::Forbidden,
            "the authenticated principal cannot issue a link challenge",
        ),
        PrincipalBindingError::Invalid { .. } => (
            IdentityBindingErrorCode::InvalidRequest,
            "identity binding request is invalid",
        ),
        PrincipalBindingError::UserAlreadyExists(_)
        | PrincipalBindingError::IncompatibleSchema
        | PrincipalBindingError::Storage
        | PrincipalBindingError::Task => (
            IdentityBindingErrorCode::ServiceUnavailable,
            "identity binding service is unavailable",
        ),
    };
    error_response(operation, code, message)
}

fn error_response(
    operation: IdentityBindingOperation,
    code: IdentityBindingErrorCode,
    message: &str,
) -> IdentityBindingResponse {
    IdentityBindingResponse::Error {
        version: IDENTITY_BINDING_PROTOCOL_VERSION,
        error: IdentityBindingError {
            code,
            operation,
            message: message.into(),
            retry_after_ms: None,
        },
    }
}
