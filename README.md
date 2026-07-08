# Sylvander v2

AI Agent framework in Rust — multi-agent, multi-session, multi-channel.

## Architecture

```
sylvander-server              binary — boots the system
  ├─ sylvander-channel-http   HTTP debug channel (SSE streaming)
  ├─ sylvander-channel-dingtalk  DingTalk bot (Stream protocol)
  ├─ sylvander-channel        Channel trait (lightweight)
  ├─ sylvander-runtime        bootstrap, session persistence
  ├─ sylvander-agent          engine, AgentRun, bus, tools, memory
  └─ sylvander-llm-anthropic  Anthropic wire protocol
```

## Quickstart

```bash
# Configure
export ANTHROPIC_API_KEY=sk-...
export SYLVANDER_MODEL=deepseek-v4-flash
# optional: DingTalk
export DINGTALK_APP_KEY=... DINGTALK_APP_SECRET=...

# Run
cargo run -p sylvander-server --release

# Test
curl -X POST http://localhost:8080/chat \
  -H 'Content-Type: application/json' \
  -d '{"session_id":"test","message":"hello"}'
```

## Key Concepts

| Layer | Crate | Responsibility |
|-------|-------|----------------|
| **Protocol** | `sylvander-llm-anthropic` | Anthropic wire format, streaming, error handling |
| **Agent Core** | `sylvander-agent` | AgentLoop, AgentRun, engine, bus, session, memory, tools, approval |
| **Runtime** | `sylvander-runtime` | Bootstrap, session persistence |
| **Channel** | `sylvander-channel` | Channel trait (lightweight contract) |
| **Channels** | `sylvander-channel-dingtalk`, `-http` | Concrete channel implementations |
| **Server** | `sylvander-server` | Binary entry point |

## Build & Test

```bash
cargo build --workspace
cargo test --workspace
```

## Conventions

- MSRV: 1.96, edition 2024
- Async: tokio (multi-thread)
- Tests: wiremock for e2e, 150+ tests
