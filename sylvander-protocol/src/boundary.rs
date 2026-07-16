//! Authentication and authorization context carried across public boundaries.
//!
//! Transports authenticate callers. The runtime authorizes their actions.
//! Keeping both facts in this protocol-owned context prevents channels from
//! inventing incompatible identity conventions.

use serde::{Deserialize, Serialize};

/// A stable, transport-independent identity for an authenticated caller.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct PrincipalId(pub String);

impl PrincipalId {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

impl std::fmt::Display for PrincipalId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// The trust domain that vouched for a principal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AuthenticationMethod {
    UnixPeer,
    BearerToken,
    WebhookSignature,
    PlatformIdentity,
    Internal,
}

/// Content-free fact that an ingress authentication attempt failed.
///
/// Credentials, request bodies, platform payloads, and provider error text are
/// deliberately absent so this value is safe to rate-limit and audit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AuthenticationFailure {
    pub attempted_method: AuthenticationMethod,
}

impl AuthenticationFailure {
    #[must_use]
    pub const fn new(attempted_method: AuthenticationMethod) -> Self {
        Self { attempted_method }
    }

    #[must_use]
    pub const fn operation(self) -> &'static str {
        match self.attempted_method {
            AuthenticationMethod::UnixPeer => "authenticate_unix_peer",
            AuthenticationMethod::BearerToken => "authenticate_bearer_token",
            AuthenticationMethod::WebhookSignature => "authenticate_webhook_signature",
            AuthenticationMethod::PlatformIdentity => "authenticate_platform_identity",
            AuthenticationMethod::Internal => "authenticate_internal",
        }
    }
}

/// The kind of actor represented by a principal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PrincipalKind {
    User,
    Channel,
    Service,
    System,
}

/// An authenticated caller. Raw credentials are deliberately never retained.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AuthenticatedPrincipal {
    pub id: PrincipalId,
    pub kind: PrincipalKind,
    pub authentication: AuthenticationMethod,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<String>,
}

impl AuthenticatedPrincipal {
    #[must_use]
    pub fn user(id: impl Into<String>, authentication: AuthenticationMethod) -> Self {
        Self {
            id: PrincipalId::new(id),
            kind: PrincipalKind::User,
            authentication,
            roles: Vec::new(),
        }
    }

    #[must_use]
    pub fn has_role(&self, role: &str) -> bool {
        self.roles.iter().any(|candidate| candidate == role)
    }
}

/// Request-scoped identity established by a transport before runtime work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BoundaryContext {
    /// Absent means authentication did not occur. Authorization must fail closed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<AuthenticatedPrincipal>,
    /// Stable configured channel instance, not merely a transport kind.
    pub channel_instance_id: String,
    /// Transport kind used for policy and audit diagnostics.
    pub transport: String,
    /// Correlation identifier generated at ingress.
    pub request_id: String,
}

impl BoundaryContext {
    #[must_use]
    pub fn authenticated(
        principal: AuthenticatedPrincipal,
        channel_instance_id: impl Into<String>,
        transport: impl Into<String>,
        request_id: impl Into<String>,
    ) -> Self {
        Self {
            principal: Some(principal),
            channel_instance_id: channel_instance_id.into(),
            transport: transport.into(),
            request_id: request_id.into(),
        }
    }

    #[must_use]
    pub fn unauthenticated(
        channel_instance_id: impl Into<String>,
        transport: impl Into<String>,
        request_id: impl Into<String>,
    ) -> Self {
        Self {
            principal: None,
            channel_instance_id: channel_instance_id.into(),
            transport: transport.into(),
            request_id: request_id.into(),
        }
    }
}

/// Stable denial categories suitable for clients, metrics, and audit queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BoundaryErrorCode {
    Unauthenticated,
    Forbidden,
    InvalidScope,
    PayloadTooLarge,
    RateLimited,
}

/// Safe public denial. It never includes a credential or sensitive resource data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BoundaryError {
    pub code: BoundaryErrorCode,
    pub operation: String,
    pub request_id: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
}

impl std::fmt::Display for BoundaryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.operation, self.message)
    }
}

impl std::error::Error for BoundaryError {}

impl BoundaryError {
    #[must_use]
    pub fn unauthenticated(context: &BoundaryContext, operation: &str) -> Self {
        Self {
            code: BoundaryErrorCode::Unauthenticated,
            operation: operation.into(),
            request_id: context.request_id.clone(),
            message: "authentication is required".into(),
            retry_after_ms: None,
        }
    }

    #[must_use]
    pub fn forbidden(context: &BoundaryContext, operation: &str) -> Self {
        Self {
            code: BoundaryErrorCode::Forbidden,
            operation: operation.into(),
            request_id: context.request_id.clone(),
            message: "the principal is not allowed to access this resource".into(),
            retry_after_ms: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_are_not_part_of_the_boundary_contract() {
        let context = BoundaryContext::authenticated(
            AuthenticatedPrincipal::user("alice", AuthenticationMethod::BearerToken),
            "desktop-primary",
            "websocket",
            "request-1",
        );
        let json = serde_json::to_value(context).expect("serialize context");
        assert_eq!(json["principal"]["id"], "alice");
        assert!(json.get("credential").is_none());
        assert!(json["principal"].get("credential").is_none());
    }

    #[test]
    fn unauthenticated_context_is_explicit() {
        let context = BoundaryContext::unauthenticated("terminal", "unix", "request-2");
        assert!(context.principal.is_none());
        assert_eq!(context.channel_instance_id, "terminal");
    }

    #[test]
    fn authentication_failure_cannot_carry_sensitive_content() {
        let failure = AuthenticationFailure::new(AuthenticationMethod::BearerToken);
        let json = serde_json::to_value(failure).unwrap();
        assert_eq!(json["attempted_method"], "bearer_token");
        assert_eq!(json.as_object().unwrap().len(), 1);
        assert_eq!(failure.operation(), "authenticate_bearer_token");
    }
}
