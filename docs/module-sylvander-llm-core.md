# Module Reference — `sylvander-llm-core`

> Provider-neutral model invocation contract for Sylvander's agent loop.
> Source: [`sylvander-llm-core/src/`](../sylvander-llm-core/src)

## 1. Purpose

`sylvander-llm-core` is the **seam** between the Agent loop and vendor-specific
LLM adapters. It defines the request shape, the streaming response shape, the
error taxonomy, and the capability validator that the runtime applies before
dispatching work. It contains **no HTTP client, vendor wire type, runtime
config, or UI protocol type** — those live in dedicated crates (e.g.
`sylvander-llm-anthropic`, see its README for the canonical provider
implementation example).

The crate is consumed by both sides:

- **Agent loop** — calls `ModelProvider::complete_stream` and consumes the
  resulting `ModelEventStream`.
- **Provider adapters** — implement `ModelProvider` for one vendor each
  (Anthropic, OpenAI, local model servers, etc.).

## 2. Public surface

```rust
// model.rs
pub struct ModelRef { pub provider: String, pub model: String }
pub struct ModelCapabilities(u16);          // bitflags
impl ModelCapabilities {
    pub const REASONING: Self; pub const PROMPT_CACHING: Self;
    pub const STRUCTURED_OUTPUT: Self; pub const TOOL_USE: Self;
    pub const VISION: Self; pub const DOCUMENT_INPUT: Self;
    pub const fn empty() -> Self;
    pub const fn contains(self, other: Self) -> bool;
    pub const fn union(self, other: Self) -> Self;
}
pub struct ModelInfo { pub reference: ModelRef, pub context_window: u32,
                        pub max_output_tokens: u32, pub capabilities: ModelCapabilities }

// provider.rs
pub type ModelEventStream =
    Pin<Box<dyn Stream<Item = Result<ModelStreamEvent, ProviderError>> + Send + 'static>>;
pub type ProviderFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ModelEventStream, ProviderError>> + Send + 'a>>;
pub type ModelCatalogFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Option<Vec<ModelInfo>>, ProviderError>> + Send + 'a>>;

#[async_trait]
pub trait ModelProvider: Send + Sync {
    fn complete_stream(&self, request: ModelRequest) -> ProviderFuture<'_>;
    fn model_catalog(&self) -> ModelCatalogFuture<'_> { Box::pin(async { Ok(None) }) }
}

// error.rs
pub enum ProviderErrorKind {
    Transport, Timeout, RateLimited, Authentication, PermissionDenied,
    ModelNotFound, InvalidRequest, Unsupported, Unavailable, Protocol,
    Cancelled, Other,
}
impl ProviderErrorKind { pub const fn is_retryable(self) -> bool; ... }
pub enum ProviderErrorPhase { Open, Stream }
pub struct ProviderError {
    pub kind: ProviderErrorKind, pub phase: ProviderErrorPhase,
    pub message: String, pub status: Option<u16>,
    pub request_id: Option<String>, pub retry_after_ms: Option<u64>,
}

// types.rs
pub enum ChatRole { User, Assistant }
pub struct ChatMessage { pub role: ChatRole, pub content: Vec<ContentBlock> }
pub enum ContentBlock {
    Text{text}, Reasoning{text, opaque_state}, ToolCall{id, name, arguments},
    ToolResult{call_id, content, is_error}, Image{image}, Document{document},
}
pub enum CacheHint { Ephemeral }
pub struct SystemInstruction { pub text: String, pub cache_hint: Option<CacheHint> }
pub struct ToolDefinition { pub name: String, pub description: String,
                            pub input_schema: serde_json::Value,
                            pub cache_hint: Option<CacheHint> }
pub struct ReasoningConfig { pub budget_tokens: u32 }
pub struct ModelRequest { pub request_id: String, pub model: ModelRef,
                          pub system: Vec<SystemInstruction>,
                          pub messages: Vec<ChatMessage>,
                          pub tools: Vec<ToolDefinition>,
                          pub max_output_tokens: u32,
                          pub reasoning: Option<ReasoningConfig>,
                          pub output_schema: Option<serde_json::Value> }
pub enum StopReason { EndTurn, ToolUse, MaxOutputTokens,
                      StopSequence(String), Refusal, Paused, Other(String) }
pub struct ModelResponse { pub id: String, pub model: ModelRef,
                           pub content: Vec<ContentBlock>,
                           pub stop_reason: StopReason, pub usage: TokenUsage }
pub enum ModelStreamEvent { TextDelta(String), ReasoningDelta(String),
                            Completed(ModelResponse) }

// usage.rs
pub struct TokenUsage {
    pub input_tokens: u64, pub output_tokens: u64,
    pub cache_write_tokens: Option<u64>, pub cache_read_tokens: Option<u64>,
}

// validation.rs
pub enum RequiredModelCapability { ToolUse, Reasoning, StructuredOutput,
                                    PromptCaching, Vision, DocumentInput }
pub enum ModelRequestFeature { ToolDefinitions, ToolHistory, ReasoningRequest,
                                ReasoningHistory, OutputSchema, SystemCacheHint,
                                ToolCacheHint, DirectImage, ToolResultImage,
                                DirectDocument, ToolResultDocument }
pub struct ModelRequestCapabilityError { pub capability: RequiredModelCapability,
                                          pub feature: ModelRequestFeature }
pub fn required_model_capabilities(request: &ModelRequest) -> ModelCapabilities;
pub fn validate_model_request_capabilities(request: &ModelRequest,
        available: ModelCapabilities) -> Result<(), ModelRequestCapabilityError>;
```

Re-exports from `lib.rs`:

```rust
pub use error::{ProviderError, ProviderErrorKind, ProviderErrorPhase};
pub use model::{ModelCapabilities, ModelInfo, ModelRef};
pub use provider::{ModelCatalogFuture, ModelEventStream, ModelProvider, ProviderFuture};
pub use types::{CacheHint, ChatMessage, ChatRole, ContentBlock, DocumentContent,
                ImageContent, MediaSource, ModelRequest, ModelResponse,
                ModelStreamEvent, OpaqueProviderState, ReasoningConfig, StopReason,
                SystemInstruction, ToolDefinition, ToolResultContent};
pub use usage::TokenUsage;
pub use validation::*;
```

## 3. Architecture

```text
              Agent loop (sylvander-agent)
                       |
                       v
       +-------------------------------+
       |   ModelProvider trait         |
       |   complete_stream(request)    |
       |       -> ModelEventStream     |
       +-------------------------------+
                  ^           ^
        implements |           | implements
                  |           |
       +-------------+   +-------------+
       | sylvander-  |   | future      |
       | llm-        |   | provider    |
       | anthropic   |   | crates      |
       +-------------+   +-------------+

   The stream contract is `Result<ModelStreamEvent, ProviderError>` where
   ModelStreamEvent is one of TextDelta(String) | ReasoningDelta(String)
   | Completed(ModelResponse). Implementations do not retry.
```

## 4. Lifecycle / data flow

A single model invocation follows this sequence:

1. **Build request.** The Agent constructs a `ModelRequest` containing
   `system` instructions, `messages`, `tools`, `max_output_tokens`,
   optional `reasoning` budget, and an optional `output_schema`.
2. **Validate capabilities.** `validate_model_request_capabilities`
   confirms the selected `ModelInfo` advertises every capability the
   request needs (tool use, reasoning, structured output, prompt
   caching, vision, document input). A failing validation returns a
   redacted `ModelRequestCapabilityError` (the error format
   intentionally excludes prompt or message text).
3. **Dispatch.** `ModelProvider::complete_stream(request)` is called.
   The provider returns a `ProviderFuture` that resolves to a
   `ModelEventStream` of `Result<ModelStreamEvent, ProviderError>`.
4. **Stream events.** The runtime iterates the stream, mapping each
   `TextDelta` / `ReasoningDelta` to a `StreamEvent` for downstream
   subscribers. A single `Completed(ModelResponse)` terminator carries
   the final `TokenUsage`.
5. **Failure path.** `ProviderError` is classified by
   `ProviderErrorKind::is_retryable()`. The Agent loop owns retry
   policy — provider adapters must not retry internally.

## 5. Configuration knobs

The crate itself does not read configuration. Provider adapters own
their own configuration (typically via `sylvander-runtime::config`):

- `ModelProvider::model_catalog` — providers with a reliable catalog
  contract can return `Some(Vec<ModelInfo>)`; otherwise `None` keeps
  the operator-managed Registry authoritative.
- `OpaqueProviderState` — provider adapters persist
  vendor-specific state (signed tokens, encrypted reasoning blocks)
  inside `ContentBlock::Reasoning` for re-feeding on subsequent
  turns. Core persists but never interprets these payloads.

## 6. Tests

| Submodule | Test file | Coverage |
|-----------|-----------|----------|
| `model` | `sylvander-llm-core/src/model.rs` (`mod tests`) | Provider-qualified identity uniqueness |
| `provider` | `sylvander-llm-core/src/provider.rs` (`mod tests`) | Object-safety, owned stream, default `model_catalog` returns `None` |
| `error` | `sylvander-llm-core/src/error.rs` (`mod tests`) | Retryability classification per kind |
| `types` | `sylvander-llm-core/src/types.rs` (`mod tests`) | Rich request/response round-trip without vendor wire types |
| `usage` | `sylvander-llm-core/src/usage.rs` (`mod tests`) | Saturating add, optional cache dims, total-input accounting |
| `validation` | `sylvander-llm-core/src/validation.rs` (`mod tests`) | Six-capability scan, history/nested-media stacking, tool-cache hint gating |

## 7. Related docs

- [`sylvander-llm-anthropic/README.md`](../sylvander-llm-anthropic/README.md) — canonical provider implementation example.
- [`docs/sylvander-agent-platform.md`](sylvander-agent-platform.md) — Agent loop that calls this crate.
- [`docs/runtime-evidence.md`](runtime-evidence.md) — runtime evidence that exercises provider adapters.
- [`AGENTS.md`](../AGENTS.md) — project-wide agent guide.

Co-Authored-By: 🦀 <oraculo@oraculo.ai>