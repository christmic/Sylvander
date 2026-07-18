//! Provider-neutral model invocation types.
//!
//! This crate owns the contract between the Agent loop and model adapters. It
//! deliberately contains no HTTP client, vendor wire type, runtime config, or
//! UI protocol type.

/// Provider-independent failure classification and retry metadata.
pub mod error;
/// Model identity, advertised capabilities, and catalog records.
pub mod model;
/// Asynchronous provider invocation and model-catalog contracts.
pub mod provider;
/// Normalized requests, responses, stream events, and multimodal content.
pub mod types;
/// Usage accounting shared across provider implementations.
pub mod usage;
/// Pre-dispatch validation of requested model capabilities.
pub mod validation;

pub use error::{ProviderError, ProviderErrorKind, ProviderErrorPhase};
pub use model::{ModelCapabilities, ModelInfo, ModelRef};
pub use provider::{ModelCatalogFuture, ModelEventStream, ModelProvider, ProviderFuture};
pub use types::{
    CacheHint, ChatMessage, ChatRole, ContentBlock, DocumentContent, ImageContent, MediaSource,
    ModelRequest, ModelResponse, ModelStreamEvent, OpaqueProviderState, ReasoningConfig,
    StopReason, SystemInstruction, ToolDefinition, ToolResultContent,
};
pub use usage::TokenUsage;
pub use validation::*;
