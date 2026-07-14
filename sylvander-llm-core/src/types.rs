//! Minimal owned request and normalized stream result types.
//!
//! The opaque payload is temporary. Strongly typed provider-neutral messages,
//! tools, system instructions, and multimodal blocks are added in the next
//! batch without changing the provider trait.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ModelRef, TokenUsage};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelRequest {
    pub request_id: String,
    pub model: ModelRef,
    pub payload: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelResponse {
    pub id: String,
    pub model: ModelRef,
    pub payload: Value,
    pub usage: TokenUsage,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ModelStreamEvent {
    TextDelta(String),
    ReasoningDelta(String),
    Completed(ModelResponse),
}
