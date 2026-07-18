# sylvander-llm-anthropic

Anthropic adapter for Sylvander's provider-neutral model contract. It owns
request conversion, authentication headers, streaming assembly, bounded
errors, and direct SDK entry points for the Anthropic Messages API.

The implemented direct API surface is:

| Endpoint | Sync | Stream | Notes |
|----------|------|--------|-------|
| `POST /v1/messages` | yes | yes (SSE) | full Message assembly |
| `POST /v1/messages/count_tokens` | yes | — | pre-flight budget check |
| `POST /v1/messages/batches` | yes | — | create batch (50% discount) |
| `GET /v1/messages/batches` | yes | — | list batches |
| `GET /v1/messages/batches/{id}` | yes | — | poll batch status |
| `POST /v1/messages/batches/{id}/cancel` | yes | — | cancel in-progress batch |

## Usage

Add to `Cargo.toml`:

```toml
[dependencies]
sylvander-llm-anthropic = { path = "../sylvander-llm-anthropic" }
```

### Non-streaming

```rust
use sylvander_llm_anthropic::prelude::*;

let client = AnthropicClient::builder()
    .api_key(std::env::var("ANTHROPIC_API_KEY")?)
    .build()?;

let msg = client.messages().create(
    CreateMessageRequest::builder()
        .model("claude-sonnet-5-20260601")
        .max_tokens(1024)
        .messages(vec![MessageParam::user("Hello")])
        .build()?
).await?;
```

### Streaming

```rust
use futures_util::StreamExt;

let mut stream = client.messages().stream(/* same request */).await?;
while let Some(event) = stream.next().await {
    let event = event?;
    if let RawStreamEvent::ContentBlockDelta { delta: ContentDelta::TextDelta { text }, .. } = &event {
        print!("{text}");
    }
    if matches!(event, RawStreamEvent::MessageStop) {
        break;
    }
}
let final_msg = stream.final_message().expect("MessageStop was seen");
```

### Sync blocking (no async)

For CLI tools / scripts:

```rust
let blocking = client.blocking()?;
let msg = blocking.messages().create(&request)?;  // no .await
```

### Count tokens

```rust
let count = client.messages().count_tokens(&request).await?;
println!("will use {} input tokens", count.input_tokens);
```

### Message Batches

```rust
let batch = client.messages().batches().create(&batch_request).await?;
// poll
let status = client.messages().batches().retrieve(&batch.id).await?;
if status.processing_status == ProcessingStatus::Ended {
    // download results_url .jsonl
}
```

## Architecture

The normative adapter lifecycle, ownership boundary, and failure contract are
in [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md). The source map below is an
orientation aid, not a second specification.

```
src/
├── lib.rs                      crate root + prelude
└── api/
    ├── client.rs               AnthropicClient + builder
    ├── error.rs                AnthropicError (thiserror)
    ├── model.rs                ModelInfo / ModelCapabilities TYPES (no values)
    ├── request.rs              CreateMessageRequest + builder
    ├── messages.rs             MessagesApi (create / stream / count_tokens)
    ├── message_stream.rs       MessageStream wrapper (impl Stream)
    ├── streaming.rs            SseParser (byte → RawStreamEvent)
    ├── batches.rs              BatchesApi (create/retrieve/list/cancel)
    ├── blocking.rs             BlockingAnthropicClient + BlockingMessagesApi
    └── types/
        ├── batch.rs            MessageBatch + variants
        ├── block.rs            ContentBlock / UserContentBlock
        ├── cache.rs            CacheControl / CacheTtl
        ├── citation.rs         TextCitation (5 variants) + Citations
        ├── event.rs            RawStreamEvent + ContentDelta + MessageDelta
        ├── image.rs            ImageBlock (base64 inline)
        ├── message.rs          MessageParam / Message / MessageTokensCount
        ├── output_config.rs    OutputConfig + JsonOutputFormat + Effort
        ├── stop_reason.rs      StopReason enum
        ├── system_prompt.rs    SystemPrompt + SystemTextBlock
        ├── thinking.rs         ThinkingConfig
        ├── tool.rs             Tool / ToolChoice / InputSchema
        ├── tool_result.rs      ToolResultBlock
        └── usage.rs            Usage
```

## Feature Coverage

- **Tools**: custom function tools only (declarative `{name, description, input_schema}`)
- **Tool choice**: `auto` / `any` / `none` / specific tool (+ `disable_parallel_tool_use`)
- **Prompt caching**: `cache_control` ephemeral breakpoint on any block
- **Extended thinking**: `thinking` config (beta header auto-attached)
- **Structured output**: `output_config` with JSON Schema (beta header auto-attached)
- **Multimodal**: base64 image blocks inline (no files API)
- **Streaming**: full SSE event surface, plus assembled `final_message()`
- **Citations**: strong-typed 5 location variants
- **Batches**: full CRUD + cancel
- **Blocking API**: sync wrappers for non-async callers

## Tests

```bash
cargo test -p sylvander-llm-anthropic --all-targets --locked
cargo clippy -p sylvander-llm-anthropic --all-targets --locked -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc -p sylvander-llm-anthropic --no-deps --locked
cargo test -p sylvander-llm-anthropic --test real_api -- --ignored
```

The last command requires explicit Anthropic credentials and is an opt-in
deployment smoke. Deterministic unit, fixture, mock-server, large-stream, and
blocking-client coverage runs without external credentials.

## Non-goals

- File uploads (Loop reads local files and base64-encodes them)
- Model listing API (model registry is `ModelInfo` type only — caller
  maintains their own)
- Anthropic Managed Agents platform (we build our own loop)
- Bedrock / Vertex / Foundry / AWS multi-backend (Anthropic API direct
  only; `base_url` is configurable for proxies)
- Sync blocking streaming (anti-pattern — 1 stream = 1 thread blocked)
- Retry / backoff (caller's responsibility)

## Conventions

- SDK is purely a protocol wrapper — no model-specific logic baked in
- All `beta`-feature headers auto-attach based on request fields
- `base_url` defaults to `https://api.anthropic.com`; override for
  proxies
- Errors classify as retryable vs permanent in `AnthropicError::is_retryable()`

## License

MIT
