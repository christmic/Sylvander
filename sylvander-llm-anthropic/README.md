# sylvander-llm-anthropic

Sylvander v2 Anthropic Protocol SDK — minimal Rust wrapper for the Anthropic
Messages API.

This is the **M1 Protocol SDK** layer. It implements only the wire format the
Agent loop actually needs:

| Endpoint | Sync | Stream |
|----------|------|--------|
| `POST /v1/messages` | yes | yes (SSE) |
| `POST /v1/messages/count_tokens` | yes | — |

Everything else (`/v1/files`, `/v1/models`, `/v1/messages/batches`,
Anthropic Managed Agents platform, Bedrock/Vertex/AWS multi-backend) is
**out of scope for v2 M1**.

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

let msg = client.messages()
    .create(
        CreateMessageRequest::builder()
            .model(ModelId::ClaudeSonnet5)
            .max_tokens(1024)
            .messages(vec![MessageParam::user("Hello")])
            .build()?,
    )
    .await?;
```

### Streaming

```rust
use futures_util::StreamExt;

let mut stream = client.messages()
    .stream(/* same request */)
    .await?;

while let Some(event) = stream.next().await {
    match event? {
        StreamEvent::ContentBlockDelta { delta: ContentDelta::TextDelta(t), .. } => {
            print!("{}", t.text);
        }
        StreamEvent::MessageStop => break,
        _ => {}
    }
}

let final_message = stream.final_message().expect("MessageStop was seen");
```

### Count tokens

```rust
let count = client.messages()
    .count_tokens(&request)
    .await?;
println!("will use {} input tokens", count.input_tokens);
```

## Feature Coverage

See `anthropic-sdk-capabilities.md` in the Oraculo repo
(`projects/Sylvander/designs/`) for the full capability surface. Highlights:

- **Tools**: custom function tools only (declarative `{name, description, input_schema}`)
- **Tool choice**: `auto` / `any` / `none` / specific tool
- **Prompt caching**: `cache_control` ephemeral breakpoint on any block
- **Extended thinking**: `thinking` config (beta header auto-attached)
- **Structured output**: `output_config` with JSON Schema (beta header auto-attached)
- **Multimodal**: base64 image blocks inline (no files API)
- **Streaming**: full SSE event surface, plus assembled `final_message()`

## Non-goals

- File uploads (Loop reads local files and base64-encodes them)
- Batch API
- Model listing API (model registry is hardcoded)
- Anthropic Managed Agents platform (we build our own loop)
- Bedrock / Vertex / Foundry / AWS multi-backend (Anthropic API direct only)
- Citations strong typing (passed through as opaque JSON)
- Sync blocking API (M2/M3 may add)

## License

MIT