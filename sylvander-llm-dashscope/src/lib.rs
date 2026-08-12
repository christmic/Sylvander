//! Typed native `DashScope` Generation SDK plus neutral provider adapter.

pub mod api;
mod convert;
mod provider;

pub use provider::{DashScopeFeatures, DashScopeProvider, DashScopeProviderConfig};

pub mod prelude {
    pub use crate::api::{
        DashScopeClient, DashScopeError, GenerationCompletion, GenerationParameters,
        GenerationRequest, GenerationStream, GenerationStreamEvent, GenerationUsage,
    };
    pub use crate::provider::{DashScopeFeatures, DashScopeProvider, DashScopeProviderConfig};
}
