//! Request-scoped model-provider credentials.
//!
//! Provider configuration is pinned by the caller. Every newly opened request
//! rechecks the credential head and receives a bounded renewable lease; no
//! provider client or unbounded secret value is cached.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use sylvander_llm_anthropic::{AnthropicProvider, api::client::AnthropicClient};
use sylvander_llm_core::{
    ModelCapabilities, ModelProvider, ModelRef, ModelRequest, ProviderError, ProviderErrorKind,
    ProviderErrorPhase, ProviderFuture, validate_model_request_capabilities,
};

use crate::credential_registry::{CredentialRegistryError, ResolvedCredential};
use crate::registry_domain::{
    CanonicalModelCapability, ModelCapabilityError, ModelDefinition, ProviderDefinition,
    parse_model_capabilities,
};

mod credential_lease;
pub(crate) use credential_lease::RegistryCredentialSource;
pub use credential_lease::{
    ExternalSecretLease, ExternalSecretLeaseError, ExternalSecretLeaseFuture,
    MAX_EXTERNAL_SECRET_LEASE_SECONDS, RenewableExternalSecretProvider, SecretLeaseMetadata,
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
    fn lease_generation(&self) -> u64 {
        self.generation()
    }
    fn expires_at_unix_secs(&self) -> i64 {
        i64::MAX
    }
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
    fn resolve_active<'a>(
        &'a self,
        provider_id: &'a str,
        binding_id: &'a str,
    ) -> CredentialLeaseFuture<'a>;
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
    #[error("provider credential lease expired")]
    Expired,
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

/// Builds a provider adapter from an already pinned registry revision.
/// Implementations must not consult mutable provider or Agent heads.
pub(crate) trait ProviderAdapterFactory: Send + Sync {
    /// Validate one pinned Provider/Model pair without resolving credentials
    /// or performing network I/O.
    fn preflight(
        &self,
        provider: &ProviderDefinition,
        model: &ModelDefinition,
    ) -> Result<(), ProviderFactoryError>;

    fn create(
        &self,
        provider: ProviderDefinition,
        credentials: Arc<dyn ActiveCredentialSource>,
    ) -> Result<Arc<dyn ModelProvider>, ProviderFactoryError>;
}

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct AnthropicProviderFactory;

impl AnthropicProviderFactory {
    /// Validate a prospective definition without consulting credential state.
    pub(crate) fn validate_definition(
        provider: &ProviderDefinition,
    ) -> Result<(), ProviderFactoryError> {
        if provider.kind != "anthropic_compatible" {
            return Err(ProviderFactoryError::UnsupportedKind);
        }
        provider
            .validate()
            .map_err(|_| ProviderFactoryError::InvalidDefinition)?;
        AnthropicClient::builder()
            .api_key("factory-validation-only")
            .base_url(&provider.base_url)
            .build()
            .map(|_| ())
            .map_err(|_| ProviderFactoryError::InvalidDefinition)
    }

    fn preflight_model(
        provider: &ProviderDefinition,
        model: &ModelDefinition,
    ) -> Result<(), ProviderFactoryError> {
        Self::validate_definition(provider)?;
        if model.provider_id != provider.id {
            return Err(ProviderFactoryError::ModelProviderMismatch);
        }
        let capabilities =
            parse_model_capabilities(&model.capabilities).map_err(|error| match error {
                ModelCapabilityError::Unknown(_) => {
                    ProviderFactoryError::UnsupportedModelCapability
                }
                _ => ProviderFactoryError::InvalidModelDefinition,
            })?;
        model
            .validate()
            .map_err(|_| ProviderFactoryError::InvalidModelDefinition)?;
        if capabilities
            .into_iter()
            .any(|capability| !anthropic_supports(capability))
        {
            return Err(ProviderFactoryError::UnsupportedModelCapability);
        }
        Ok(())
    }
}

impl ProviderAdapterFactory for AnthropicProviderFactory {
    fn preflight(
        &self,
        provider: &ProviderDefinition,
        model: &ModelDefinition,
    ) -> Result<(), ProviderFactoryError> {
        Self::preflight_model(provider, model)
    }

    fn create(
        &self,
        provider: ProviderDefinition,
        credentials: Arc<dyn ActiveCredentialSource>,
    ) -> Result<Arc<dyn ModelProvider>, ProviderFactoryError> {
        Self::validate_definition(&provider)?;

        Ok(Arc::new(RequestScopedAnthropicProvider::new(
            provider.id,
            provider.revision,
            provider.base_url,
            provider.credential_binding_id,
            credentials,
        )))
    }
}

/// Stable, content-free factory failures safe for protocol and log surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ProviderFactoryError {
    #[error("provider kind is unsupported")]
    UnsupportedKind,
    #[error("provider definition is invalid")]
    InvalidDefinition,
    #[error("model definition is invalid")]
    InvalidModelDefinition,
    #[error("model provider does not match adapter definition")]
    ModelProviderMismatch,
    #[error("model capability is unsupported by provider adapter")]
    UnsupportedModelCapability,
}

const fn anthropic_supports(capability: CanonicalModelCapability) -> bool {
    match capability {
        CanonicalModelCapability::ExtendedThinking
        | CanonicalModelCapability::PromptCaching
        | CanonicalModelCapability::StructuredOutput
        | CanonicalModelCapability::ToolUse
        | CanonicalModelCapability::Vision
        | CanonicalModelCapability::DocumentInput => true,
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
                .resolve_active(&self.provider_id, &self.credential_binding_id)
                .await
                .map_err(map_credential_error)?;
            let credential_generation = lease.generation();
            let lease_generation = lease.lease_generation();
            let lease_expires_at = lease.expires_at_unix_secs();
            tracing::debug!(
                provider = %self.provider_id,
                credential_generation,
                lease_generation,
                lease_expires_at,
                "provider credential lease opened"
            );
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

/// Immutable, fail-closed routing table for one pinned Agent revision.
///
/// The router never chooses an alternate Provider or Model. A request must
/// match the exact qualified allowlist before its Provider adapter is called.
pub(crate) struct PinnedProviderRouter {
    routes: HashMap<String, Arc<dyn ModelProvider>>,
    model_catalog: HashMap<ModelRef, ModelCapabilities>,
}

impl PinnedProviderRouter {
    pub(crate) fn new(
        routes: HashMap<String, Arc<dyn ModelProvider>>,
        model_catalog: HashMap<ModelRef, ModelCapabilities>,
    ) -> Result<Self, ProviderRouterBuildError> {
        if routes.is_empty() {
            return Err(ProviderRouterBuildError::EmptyRoutes);
        }
        if model_catalog.is_empty() {
            return Err(ProviderRouterBuildError::EmptyModels);
        }
        if routes
            .keys()
            .any(|provider_id| provider_id.trim().is_empty())
            || model_catalog
                .keys()
                .any(|model| model.provider.trim().is_empty() || model.model.trim().is_empty())
        {
            return Err(ProviderRouterBuildError::IncompleteCatalog);
        }

        let used_routes = model_catalog
            .keys()
            .map(|model| model.provider.as_str())
            .collect::<HashSet<_>>();
        if model_catalog
            .keys()
            .any(|model| !routes.contains_key(&model.provider))
            || routes
                .keys()
                .any(|provider_id| !used_routes.contains(provider_id.as_str()))
        {
            return Err(ProviderRouterBuildError::IncompleteCatalog);
        }

        Ok(Self {
            routes,
            model_catalog,
        })
    }
}

impl std::fmt::Debug for PinnedProviderRouter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PinnedProviderRouter")
            .field("route_count", &self.routes.len())
            .field("model_count", &self.model_catalog.len())
            .finish()
    }
}

impl ModelProvider for PinnedProviderRouter {
    fn complete_stream(&self, request: ModelRequest) -> ProviderFuture<'_> {
        let selected = self
            .routes
            .get(&request.model.provider)
            .cloned()
            .zip(self.model_catalog.get(&request.model).copied());
        Box::pin(async move {
            let (route, capabilities) = selected.ok_or_else(router_rejection)?;
            validate_model_request_capabilities(&request, capabilities)
                .map_err(|_| router_capability_rejection())?;
            route.complete_stream(request).await
        })
    }
}

/// Content-free construction failures safe for logs and protocol mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ProviderRouterBuildError {
    #[error("provider router requires at least one route")]
    EmptyRoutes,
    #[error("provider router requires at least one qualified Model")]
    EmptyModels,
    #[error("provider router catalog is incomplete")]
    IncompleteCatalog,
}

fn map_credential_error(error: CredentialAccessError) -> ProviderError {
    match error {
        CredentialAccessError::Unavailable
        | CredentialAccessError::InvalidEncoding
        | CredentialAccessError::Expired => provider_error(
            ProviderErrorKind::Authentication,
            "provider credential unavailable",
        ),
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

fn router_rejection() -> ProviderError {
    provider_error(
        ProviderErrorKind::InvalidRequest,
        "requested model is unavailable for this Agent revision",
    )
}

fn router_capability_rejection() -> ProviderError {
    provider_error(
        ProviderErrorKind::Unsupported,
        "requested model does not support required capabilities",
    )
}

#[cfg(test)]
#[path = "../tests/unit/request_scoped_provider.rs"]
mod tests;
