# sylvander-agent

Sylvander Agent Loop — async reactive driver that calls one exact
provider-qualified model route, executes tools, re-feeds results, and emits
events as the loop progresses.

This crate is the Agent execution layer. It builds on the provider-neutral
model contract and contains the iterative model/tool loop, authenticated run
lifecycle, durable session store, prompt composition, tools, workspace
executors, Skills, and supervised MCP stdio.

## Current scope

- `AgentLoop` for provider-qualified iterative generation and tool re-feeding
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
sylvander-llm-core = { path = "../sylvander-llm-core" }
```

### Quickstart

```rust,no_run
use std::sync::Arc;

use sylvander_agent::{
    prelude::{AgentLoop, MessageParam, ToolContext},
    tool_context::Cap,
};
use sylvander_llm_anthropic::{
    AnthropicProvider,
    api::{
        client::AnthropicClient,
        model::{ModelCapabilities, ModelInfo},
    },
};
use sylvander_llm_core::{
    ModelCapabilities as ProviderCapabilities, ModelInfo as ProviderModelInfo, ModelRef,
};

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
    let exact_model = ProviderModelInfo {
        reference: ModelRef::new("anthropic", model.id.clone()),
        context_window: model.context_window,
        max_output_tokens: model.max_output_tokens,
        capabilities: ProviderCapabilities::TOOL_USE,
    };

    let mut loop_ = AgentLoop::builder()
        .qualified_router(Arc::new(AnthropicProvider::new("anthropic", client)))
        .provider_model(exact_model)
        .tool_context(
            ToolContext::new(sylvander_protocol::SessionContext::new(
                "user", "agent", "session",
            ))
            .with_fs_root("/tmp")
            .with_capability(Cap::Read),
        )
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
        AgentEvent::Compressed { layers } => {
            eprintln!(
                "[compressed, dropped {} messages]",
                total_removed(&layers)
            )
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
    .qualified_router(router)
    .provider_model(exact_model)
    .tool_context(
        ToolContext::new(sylvander_protocol::SessionContext::new(
            "user", "agent", "session",
        ))
        .with_fs_root("/workspace")
        .with_capability(sylvander_agent::tool_context::Cap::Read),
    )
    .tool(ProjectSummary)
    .build()?;
```

Standalone `AgentLoop` embeddings receive an exact-registry gateway. Production
`Runtime` replaces it with the actor-aware policy and durable audit gateway.
Built-ins, dynamic MCP tools, browser/host adapters, and extensions therefore
share one authorization entry immediately before execution.

### Custom compression pipeline

```rust,ignore
let pipeline = CompressionPipeline::builder()
    .layer(MyCompressionLayer)
    .build();

let loop_ = AgentLoop::builder()
    .qualified_router(router)
    .provider_model(exact_model)
    .tool_context(ToolContext::new(
        sylvander_protocol::SessionContext::new("user", "agent", "session"),
    ))
    .compression_pipeline(pipeline)
    .build()?;
```

Custom layers implement the object-safe `CompressionLayer` contract and return
a typed `LayerReport`. A layer records a bounded failure instead of aborting
later layers in the pipeline.

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
Compressed { layers: Vec<LayerReport> }     compression pipeline reported work
IterationEnd { iteration, usage }     iteration done
Done(Message)                         loop terminated cleanly
Error(String)                         loop terminated with error
```

## API Reference

| Method | Signature | Description |
|---|---|---|
| `run(initial)` | `async` | Drive loop, return `Result<AgentLoopResult, _>` — convenience over `run_stream` |
| `run_with_events(initial, callback)` | `async` | Drive loop, fire non-terminal events into callback, return final `AgentLoopResult` |
| `run_stream(initial)` | `-> impl Stream<Item = AgentEvent>` | Core API — drive loop, yield events as they happen |
| `model()` | `-> &ModelInfo` | Resolved model metadata |
| `tools()` | `-> &ToolRegistry` | Configured tool registry |
| `max_iterations()` | `-> u32` | Configured cap |
| `max_retries()` | `-> u32` | Configured retry count |

### Builder methods

| Builder method | Default | Description |
|---|---|---|
| `qualified_router(router)` | required | Immutable provider-qualified model router |
| `provider_model(model_info)` | required | Exact `(provider, model)` metadata and capabilities |
| `tool(tool)` | none | Register a single tool (chainable) |
| `tools(registry)` | empty | Replace tool registry |
| `compression_pipeline(pipeline)` | model default | Replace the ordered compression pipeline |
| `max_iterations(n)` | 50 | Iteration cap |
| `max_retries(n)` | 3 | Retry transient provider-open failures on the same exact route; 0 = disable |

`AgentLoopResult { final_message, iterations, total_usage }`.

## Error types

| Variant | When |
|---|---|
| `MaxIterationsReached(u32)` | Loop hit the iteration cap |
| `IncompatibleModel(String)` | Request requires capability the model lacks |
| `Provider { attempts, source }` | Qualified provider route failed after the recorded attempts |
| `Tool(String)` | Non-recoverable tool failure |
| `Compression(String)` | Compressor reported an error |
| `Validation(String)` | Bad request shape |
| `Builder(String)` | Builder field missing |

`is_retryable()` delegates only to the provider-neutral `ProviderError`
classification; the other variants are deterministic caller or execution
contract failures.

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
