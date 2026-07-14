//! Provider-neutral model invocation types.
//!
//! This crate owns the contract between the Agent loop and model adapters. It
//! deliberately contains no HTTP client, vendor wire type, runtime config, or
//! UI protocol type.

pub mod error;
pub mod model;
pub mod provider;
pub mod types;
pub mod usage;

pub use error::{ProviderError, ProviderErrorKind, ProviderErrorPhase};
pub use model::{ModelCapabilities, ModelInfo, ModelRef};
pub use provider::{ModelEventStream, ModelProvider, ProviderFuture};
pub use types::{ModelRequest, ModelResponse, ModelStreamEvent};
pub use usage::TokenUsage;
