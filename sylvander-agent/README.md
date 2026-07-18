# sylvander-agent

Sylvander v2 Agent Loop — async reactive driver that calls the Anthropic
Messages API, executes tools, re-feeds results, and emits events as the
loop progresses.

This crate is the Agent execution layer. It builds on the provider-neutral
model contract and contains the iterative model/tool loop, authenticated run
lifecycle, durable session store, prompt composition, tools, workspace
executors, Skills, and supervised MCP stdio.

## Current scope

- `AgentLoop` for provider-compatible iterative generation and tool re-feeding
- `AgentRun` / `AgentRunEngine` for authenticated per-session execution
- `ToolRegistry`, concrete workspace/memory/plan/task tools, and approvals
- durable SQLite sessions, composition, compression, Skills, and MCP stdio
- location-neutral workspace executors and isolated local worktree journaling

The authoritative ownership and extension rules are in
[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md); this README keeps direct Rust
usage examples.

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

Use `run_with_events` to react to events as they happen (text chunks,
tool calls, compression, etc.):

```rust,no_run
use sylvander_agent::prelude::*;

# async fn example(loop_: AgentLoop) -> Result<(), Box<dyn std::error::Error>> {
let mut loop_ = loop_;
let run = loop_.run_with_events(
    vec![MessageParam::user("hi")],
    |event| match event {
        AgentEvent::TextChunk(t) => print!("{t}"),
        AgentEvent::ToolCallStart { name, .. } => eprintln!("\n[tool] {name}"),
        AgentEvent::Compressed { removed_count, .. } => {
            eprintln!("[compressed, dropped {removed_count} messages]")
        }
        _ => {}
    },
).await?;
# Ok(())
# }
```

`run_with_events` fires **non-terminal** events into the callback
(`IterationStart`, `TextChunk`, `ToolCallStart`, `ToolCallEnd`,
`Compressed`, `IterationEnd`). The terminal `Done` event is
extracted into the returned `AgentRun`; terminal `Error` is
returned as the `Err` variant. This avoids double-handling.

### Pull from a stream directly

For `select!`, timeout cancellation, or merging multiple agents,
pull from `run_stream()`:

```rust,no_run
use futures_util::StreamExt;
use sylvander_agent::prelude::*;

# async fn example(loop_: AgentLoop) -> Result<(), Box<dyn std::error::Error>> {
let mut loop_ = loop_;
let mut stream = Box::pin(loop_.run_stream(vec![MessageParam::user("hi")]));
while let Some(event) = stream.next().await {
    // Full control — including `select!` over other futures
    # let _ = event;
}
# Ok(())
# }
```

### Custom tools

Implement `Tool`, declare the operation's security class, and use the
Runtime-derived `ToolContext` for identity, workspace, executor, budget, and
capability checks. Model input never selects an owner or an unrestricted host
path:

```rust,ignore
struct ProjectSummary;

#[async_trait]
impl Tool for ProjectSummary {
    fn name(&self) -> &'static str { "project_summary" }
    fn description(&self) -> &'static str { "Read the bounded project summary" }
    fn input_schema(&self) -> InputSchema {
        InputSchema::new_with_properties(json!({}), &[])
    }

    fn invocation_class(&self) -> ToolInvocationClass {
        ToolInvocationClass::Read
    }

    async fn execute(
        &self,
        ctx: &ToolContext,
        _input: JsonValue,
    ) -> Result<ToolOutput, ToolError> {
        if !ctx.has_cap(Cap::Read) {
            return Ok(ToolOutput::err("read capability not granted"));
        }
        let root = ctx.surface.fs_root.as_ref()
            .ok_or_else(|| ToolError::Other("workspace unavailable".into()))?;
        let content = std::fs::read_to_string(root.join("PROJECT.md"))
            .map_err(|error| ToolError::Other(error.to_string()))?;
        Ok(ToolOutput::ok(content))
    }
}

let mut loop_ = AgentLoop::builder()
    .client(client)
    .model(model)
    .tool(ProjectSummary)
    .build()?;
```

Standalone `AgentLoop` embeddings receive an exact-registry gateway. Production
`Runtime` replaces it with the actor-aware policy and durable audit gateway.
Built-ins, dynamic MCP tools, browser/host adapters, and extensions therefore
share one authorization entry immediately before execution.

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

Runtime contracts:

- [Skill package format, activation, and health](docs/skills.md)
- [MCP supervision, cancellation, and inspection](docs/mcp.md)
- [Workspace executor and coding-tool contract](docs/workspace-execution.md)

The detailed source ownership map is maintained in
[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md). Tests live under `tests/`;
production modules contain only path-qualified test-module declarations.

### Iteration loop

```text
run() {
    for iteration in 1..=max_iterations {
        emit(IterationStart { iteration });

        // 1. Compose typed, budgeted context with provenance.
        // 2. Freeze the exact tool/Skill/MCP capability revision.
        // 3. Build and validate the provider-neutral model request.
        // 4. Stream through the selected provider with bounded retry.
        // 5. Emit TextChunk / ThinkingChunk from response content.
        // 6. Re-feed the assistant message.
        // 7. stop_reason match:
        //    EndTurn / StopSequence / MaxTokens / Refusal / PauseTurn → break
        //    ToolUse → approval → policy/audit gateway → execute
        //              → terminal audit → re-feed bounded tool_result blocks

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

| Method | Signature | Description |
|---|---|---|
| `run(initial)` | `async` | Drive loop, return `Result<AgentRun, _>` — convenience over `run_stream` |
| `run_with_events(initial, callback)` | `async` | Drive loop, fire non-terminal events into callback, return final `AgentRun` |
| `run_stream(initial)` | `-> impl Stream<Item = AgentEvent>` | Core API — drive loop, yield events as they happen |
| `model()` | `-> &ModelInfo` | Resolved model metadata |
| `tools()` | `-> &ToolRegistry` | Configured tool registry |
| `max_iterations()` | `-> u32` | Configured cap |
| `max_retries()` | `-> u32` | Configured retry count |

### Builder methods

| Builder method | Default | Description |
|---|---|---|
| `client(client)` | required | Anthropic SDK client |
| `model(model_info)` | required | Resolved `ModelInfo` (capabilities + context_window) |
| `tool(tool)` | none | Register a single tool (chainable) |
| `tools(registry)` | empty | Replace tool registry |
| `max_iterations(n)` | 50 | Iteration cap |
| `max_retries(n)` | 3 | Per-LLM-call retry on transient errors; 0 = disable |

Note: the previous `on_event(f)` builder method was removed in the
stream-first refactor — events are now delivered through
`run_with_events(initial, callback)` or by pulling from
`run_stream(initial)` directly.

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

## Workspace rollback journal

When `AgentRunBuilder::workspace_journal` is configured, successful built-in
`Write` and `Edit` calls record durable pre/post snapshots grouped by Agent
turn. `preview_workspace_rollback` performs conflict checks without mutation;
`rollback_workspace_latest` requires that previewed turn id and restores the
whole group in reverse order. The journal rejects path escapes, symlink hops,
oversized files, active turns, stale confirmations, and external changes. It
does not claim to capture shell commands or user edits.

## Tests

```bash
cargo test -p sylvander-agent --locked
cargo test -p sylvander-agent --all-targets --locked
cargo clippy -p sylvander-agent --all-targets --locked -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc -p sylvander-agent --no-deps --locked
```

Unit, contract, fixture-provider, and opt-in real-provider journeys are all
owned by `tests/`; CI does not depend on live provider credentials.

## Conventions

- Async and streaming first; cancellation is part of the execution contract.
- Runtime owns identity, workspace routing, durable stores, and authority.
- Model compatibility is validated before dispatch.
- Tool output, context, and evidence have explicit size and content policies.
- No compatibility fallback is retained unless an approved migration names
  its source schema and transition.

## License

MIT
