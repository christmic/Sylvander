//! Typed `OpenAI` Responses and Chat Completions SDK plus neutral provider adapter.
//!
//! Runtime supplies endpoint, credentials, protocol, and compatible-provider
//! feature switches explicitly. No process environment discovery occurs here.

pub mod api;
mod convert;
mod provider;

pub use provider::{OpenAiProtocol, OpenAiProvider, OpenAiProviderConfig, ProviderFeatures};

pub mod prelude {
    pub use crate::api::chat::{
        ChatCompletion, ChatCompletionChunk, ChatCompletionStream, ChatCompletionUsage,
        ChatStreamEvent, CreateChatCompletionRequest,
    };
    pub use crate::api::responses::{
        CreateResponseRequest, Response, ResponseOutputItem, ResponseStreamEvent, ResponseUsage,
        ResponsesStream,
    };
    pub use crate::api::{OpenAiClient, OpenAiError};
    pub use crate::provider::{
        OpenAiProtocol, OpenAiProvider, OpenAiProviderConfig, ProviderFeatures,
    };
}
