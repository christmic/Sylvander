//! Direct typed support for the `OpenAI` Responses and Chat Completions APIs.

pub mod chat;
mod client;
mod error;
pub mod responses;
mod sse;

pub use client::OpenAiClient;
pub use error::OpenAiError;
