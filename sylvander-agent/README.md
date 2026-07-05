# sylvander-agent

Sylvander v2 Agent Loop — async reactive driver that calls the Anthropic
Messages API, executes tools, re-feeds results, and emits events as the
loop progresses.

This is the **M2 Agent Loop** layer. It builds on the M1 protocol SDK
(`sylvander-llm-anthropic`) and provides the iteration framework.

## Scope (M2)

- `AgentLoop` struct with builder (OOP class-based)
- Reactive event stream (`AgentEvent` + `on_event` callback)
- `Tool` trait + `ToolRegistry` (caller plugs in their own tools)
- `Compressor` trait + simple default impl
- Retry / backoff + capability validation + iteration limit
- **No concrete tools** (Read/Bash/Edit) — those land in M3

## Usage

Add to `Cargo.toml`:

```toml
[dependencies]
sylvander-agent = { path = "../sylvander-agent" }
sylvander-llm-anthropic = { path = "../sylvander-llm-anthropic" }
```

### Quickstart

```rust,no_run
use sylvander_agent::prelude::*;
use sylvander_llm_anthropic::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Caller builds their own model registry (per C11 architecture).
    let model = ModelInfo::builder()
        .id("claude-sonnet-5-20260601")
        .context_window(200_000)
        .max_output_tokens(32_000)
        .capability(ModelCapabilities::TOOL_USE)
        .build()
        .unwrap();

    let client = AnthropicClient::builder()
        .api_key(std::env::var("ANTHROPIC_API_KEY")?)
        .build()?;

    let mut loop_ = AgentLoop::builder()
        .client(client)
        .model(model)
        .max_iterations(50)
        .build()?;

    let run = loop_.run(vec![MessageParam::user("List files in /tmp")]).await?;
    println!("finished after {} iterations", run.iterations);
    Ok(())
}
```

### Reactive event stream

Use `on_event` to react to events as they happen (text chunks, tool
calls, compression, etc.):

```rust,no_run
use sylvander_agent::prelude::*;

# async fn example(loop_: AgentLoop) -> Result<(), Box<dyn std::error::Error>> {
let mut loop_ = loop_;
let run = loop_.run_with_events(vec![MessageParam::user("hi"))], |event| {
    match event {
        AgentEvent::TextChunk(t) => print!("{t}"),
        AgentEvent::ToolCallStart { name, .. } => eprintln!("\n[tool] {name}"),
        AgentEvent::Compressed { removed_count, .. } => {
            eprintln!("[compressed, dropped {removed_count} messages]")
        }
        AgentEvent::Done(_) => eprintln!("\n[done]"),
        _ => {}
    }
}).await?;
# Ok(())
# }
```

### Custom tools (M3+)

M2 ships `MockTool` for tests. To add a real tool, implement the
`Tool` trait:

```rust,ignore
struct ReadTool { workdir: PathBuf }

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &str { "Read" }
    fn description(&self) -> &str { "Read a file from disk" }
    fn input_schema(&self) -> InputSchema {
        InputSchema::new_with_properties(
            json!({"file_path": {"type": "string"}}),
            &["file_path"],
        )
    }
    async fn execute(&self, input: JsonValue) -> Result<ToolOutput, ToolError> {
        let path = input["file_path"].as_str().unwrap();
        let content = std::fs::read_to_string(self.workdir.join(path))
            .map_err(|e| ToolError::Other(e.to_string()))?;
        Ok(ToolOutput::ok(content))
    }
}

let mut loop_ = AgentLoop::builder()
    .client(client)
    .model(model)
    .tool(ReadTool { workdir: ".".into() })
    .build()?;
```

### Custom compression strategy

```rust,ignore
struct MyCompressor;
impl Compressor for MyCompressor {
    fn maybe_compress(&self, ctx: &mut CompressContext) -> CompressionOutcome {
        // Your strategy here
        CompressionOutcome::Keep
    }
}

let loop_ = AgentLoop::builder()
    .compressor(MyCompressor)
    // ...
    .build()?;
```

## Architecture

```
src/
├── lib.rs          # crate root + prelude
├── error.rs        # AgentLoopError (thiserror)
├── event.rs        # AgentEvent enum (reactive events)
├── compress.rs     # Compressor trait + NoCompression + SimpleWindowCompressor
├── tool.rs         # Tool trait + ToolRegistry + MockTool
└── loop_.rs        # AgentLoop + AgentLoopBuilder + AgentRun
```

### Iteration loop

```text
run() {
    for iteration in 1..=max_iterations {
        emit(IterationStart { iteration });

        // 1. Compressor.maybe_compress → emit Compressed if truncated
        // 2. Build CreateMessageRequest from current messages + tools
        // 3. validate_capabilities(&request)
        // 4. call_llm_with_retry(&request)   # exponential backoff on 5xx/429
        // 5. Emit TextChunk / ThinkingChunk from response.content
        // 6. Re-feed assistant message
        // 7. stop_reason match:
        //    EndTurn / StopSequence / MaxTokens / Refusal / PauseTurn → break
        //    ToolUse → execute tools, re-feed tool_result blocks

        emit(IterationEnd { iteration, usage });
    }

    if no end → MaxIterationsReached
    emit(Done);
}
```

### Event types

```text
IterationStart { iteration }           loop starting this iteration
TextChunk(String)                     text delta from model
ThinkingChunk(String)                 thinking delta (when enabled)
ToolCallStart { id, name, input }     tool about to execute
ToolCallEnd { id, name, output, is_error }
Compressed { removed_count, freed_tokens }   compressor fired
IterationEnd { iteration, usage }     iteration done
Done(Message)                         loop terminated cleanly
Error(String)                         loop terminated with error
```

## API Reference

| Builder method | Default | Description |
|---|---|---|
| `client(client)` | required | Anthropic SDK client |
| `model(model_info)` | required | Resolved `ModelInfo` (capabilities + context_window) |
| `tool(tool)` | none | Register a single tool (chainable) |
| `tools(registry)` | empty | Replace tool registry |
| `compressor(c)` | `NoCompression` | Compression strategy |
| `max_iterations(n)` | 50 | Iteration cap |
| `max_retries(n)` | 3 | Per-LLM-call retry on transient errors; 0 = disable |
| `on_event(f)` | none | Reactive event callback |

`run(initial_messages)` returns `Result<AgentRun, AgentLoopError>`.
`AgentRun { final_message, iterations, total_usage }`.

## Error types

| Variant | When |
|---|---|
| `MaxIterationsReached(u32)` | Loop hit the iteration cap |
| `IncompatibleModel(String)` | Request requires capability the model lacks |
| `Llm { retries, source }` | LLM call failed (after retries if `retries > 0`) |
| `Tool(String)` | Non-recoverable tool failure |
| `Compression(String)` | Compressor reported an error |
| `Validation(String)` | Bad request shape |
| `Builder(String)` | Builder field missing |

`is_retryable()` on the error delegates to the inner `AnthropicError`
for the `Llm` variant; other variants are deterministic caller bugs.

## Tests

```bash
cargo test --workspace                # all 210 tests across both crates
cargo test -p sylvander-agent          # 51 M2 tests
cargo test -p sylvander-agent --lib    # 34 unit
cargo test -p sylvander-agent --test simple_run       # 7 wiremock integration
cargo test -p sylvander-agent --test capability_retry # 9 wiremock integration
```

Test breakdown (51 total):
- 34 unit (compress.rs / tool.rs / error.rs / event.rs / loop_.rs)
- 7 wiremock integration (`tests/simple_run.rs`)
- 9 wiremock integration (`tests/capability_retry.rs`)
- 1 doctest

Wiremock is the integration test backbone — no real API calls in CI.

## Conventions

- Class-based OOP — `AgentLoop` is a struct, no FP combinators
- Reactive events — `on_event` callback delivers events as they fire
- Async-first — sync blocking API deferred
- Capability validation before LLM call — fast-fails on model mismatch
- Composable compression — `Compressor` trait, simple default provided

## Non-goals (M3+)

- Concrete tools (Read/Bash/Edit/Glob/Grep) — M3
- Parallel tool execution — M3
- Permission system / approval gates — M3
- MCP integration — M3
- Sandbox / process isolation — M3
- Sub-agents / Hooks / Skills — M4
- Long-term memory — M4
- Self-improvement — M4
- Sync blocking loop — skipped
- Full reactive streaming (use `on_event` instead) — M3 enhancement

## License

MIT