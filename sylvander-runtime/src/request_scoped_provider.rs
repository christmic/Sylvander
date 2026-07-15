//! Request-scoped model-provider credentials.
//!
//! Provider configuration is pinned by the caller. Only the credential head
//! is resolved for each newly opened request; no client or secret is cached.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use sylvander_llm_anthropic::{AnthropicProvider, api::client::AnthropicClient};
use sylvander_llm_core::{
    ModelProvider, ModelRequest, ProviderError, ProviderErrorKind, ProviderErrorPhase,
    ProviderFuture,
};

use crate::agent_registry::AgentRegistry;
use crate::credential_registry::{
    CredentialRegistryError, CredentialSecretResolver, ResolvedCredential,
};

pub(crate) type CredentialLeaseFuture<'a> = Pin<
    Box<
        dyn Future<Output = Result<Box<dyn ActiveCredentialLease>, CredentialAccessError>>
            + Send
            + 'a,
    >,
>;

/// Short-lived access to one resolved credential generation.
pub(crate) trait ActiveCredentialLease: Send {
    fn generation(&self) -> u64;
    fn secret(&self) -> Result<&str, CredentialAccessError>;
}

impl ActiveCredentialLease for ResolvedCredential {
    fn generation(&self) -> u64 {
        self.generation()
    }

    fn secret(&self) -> Result<&str, CredentialAccessError> {
        self.value()
            .as_str()
            .map_err(|_| CredentialAccessError::InvalidEncoding)
    }
}

/// Object-safe request boundary, allowing registry-backed and test sources.
pub(crate) trait ActiveCredentialSource: Send + Sync {
    fn resolve_active<'a>(&'a self, binding_id: &'a str) -> CredentialLeaseFuture<'a>;
}

#[derive(Clone)]
pub(crate) struct RegistryCredentialSource {
    registry: AgentRegistry,
    resolver: Arc<dyn CredentialSecretResolver>,
}

impl RegistryCredentialSource {
    pub(crate) fn new(
        registry: AgentRegistry,
        resolver: Arc<dyn CredentialSecretResolver>,
    ) -> Self {
        Self { registry, resolver }
    }
}

impl std::fmt::Debug for RegistryCredentialSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RegistryCredentialSource")
            .finish_non_exhaustive()
    }
}

impl ActiveCredentialSource for RegistryCredentialSource {
    fn resolve_active<'a>(&'a self, binding_id: &'a str) -> CredentialLeaseFuture<'a> {
        Box::pin(async move {
            self.registry
                .resolve_active_credential(binding_id, self.resolver.as_ref())
                .await
                .map(|credential| Box::new(credential) as Box<dyn ActiveCredentialLease>)
                .map_err(CredentialAccessError::from_registry)
        })
    }
}

/// Redacted classification only; it deliberately carries no registry cause.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum CredentialAccessError {
    #[error("provider credential unavailable")]
    Unavailable,
    #[error("credential registry unavailable")]
    RegistryUnavailable,
    #[error("credential registry integrity failure")]
    Integrity,
    #[error("provider credential has invalid encoding")]
    InvalidEncoding,
}

impl CredentialAccessError {
    fn from_registry(error: CredentialRegistryError) -> Self {
        match error {
            CredentialRegistryError::Registry(
                crate::agent_registry::AgentRegistryError::Storage(_)
                | crate::agent_registry::AgentRegistryError::Task(_),
            ) => Self::RegistryUnavailable,
            CredentialRegistryError::Registry(
                crate::agent_registry::AgentRegistryError::Integrity(_)
                | crate::agent_registry::AgentRegistryError::Serialization(_),
            ) => Self::Integrity,
            _ => Self::Unavailable,
        }
    }
}

/// Anthropic adapter whose immutable provider definition is session-pinned.
pub(crate) struct RequestScopedAnthropicProvider {
    provider_id: String,
    provider_revision: u64,
    base_url: String,
    credential_binding_id: String,
    credentials: Arc<dyn ActiveCredentialSource>,
}

impl RequestScopedAnthropicProvider {
    pub(crate) fn new(
        provider_id: impl Into<String>,
        provider_revision: u64,
        base_url: impl Into<String>,
        credential_binding_id: impl Into<String>,
        credentials: Arc<dyn ActiveCredentialSource>,
    ) -> Self {
        Self {
            provider_id: provider_id.into(),
            provider_revision,
            base_url: base_url.into(),
            credential_binding_id: credential_binding_id.into(),
            credentials,
        }
    }
}

impl std::fmt::Debug for RequestScopedAnthropicProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RequestScopedAnthropicProvider")
            .field("provider_id", &self.provider_id)
            .field("provider_revision", &self.provider_revision)
            .finish_non_exhaustive()
    }
}

impl ModelProvider for RequestScopedAnthropicProvider {
    fn complete_stream(&self, request: ModelRequest) -> ProviderFuture<'_> {
        Box::pin(async move {
            if request.model.provider != self.provider_id {
                return Err(provider_error(
                    ProviderErrorKind::InvalidRequest,
                    "model provider does not match adapter",
                ));
            }

            let lease = self
                .credentials
                .resolve_active(&self.credential_binding_id)
                .await
                .map_err(map_credential_error)?;
            let _generation = lease.generation();
            let client = AnthropicClient::builder()
                .api_key(lease.secret().map_err(map_credential_error)?)
                .base_url(&self.base_url)
                .build()
                .map_err(|_| {
                    provider_error(
                        ProviderErrorKind::InvalidRequest,
                        "provider configuration is invalid",
                    )
                })?;
            drop(lease);

            AnthropicProvider::new(&self.provider_id, client)
                .complete_stream(request)
                .await
        })
    }
}

fn map_credential_error(error: CredentialAccessError) -> ProviderError {
    match error {
        CredentialAccessError::Unavailable | CredentialAccessError::InvalidEncoding => {
            provider_error(
                ProviderErrorKind::Authentication,
                "provider credential unavailable",
            )
        }
        CredentialAccessError::RegistryUnavailable => provider_error(
            ProviderErrorKind::Unavailable,
            "credential registry unavailable",
        ),
        CredentialAccessError::Integrity => provider_error(
            ProviderErrorKind::Protocol,
            "credential registry integrity failure",
        ),
    }
}

fn provider_error(kind: ProviderErrorKind, message: &'static str) -> ProviderError {
    ProviderError::new(kind, ProviderErrorPhase::Open, message)
}

#[cfg(test)]
#[path = "request_scoped_provider_tests.rs"]
mod tests;
