# Sylvander v2

AI Agent framework in Rust — multi-agent, multi-session, multi-channel,
with structured tool approval and user clarification.

## Architecture

The normative server-Agent architecture, implementation audit, and ordered
production backlog live in
[`docs/sylvander-agent-platform.md`](docs/sylvander-agent-platform.md). Crate
presence does not by itself mean that an adapter is wired into the production
server; the audit records that distinction explicitly.
Deployment configuration is documented in
[`docs/server-configuration.md`](docs/server-configuration.md).

```
sylvander-server                  binary — boots the system
  ├─ sylvander-channel-http       HTTP debug channel (SSE streaming)
  ├─ sylvander-channel-ws         WebSocket channel (desktop + approval UI)
  ├─ sylvander-channel-unix       Unix socket (CLI/TUI clients, line JSON)
  ├─ sylvander-channel-dingtalk   DingTalk bot (Stream protocol)
  ├─ sylvander-channel-telegram   Telegram bot (webhook + sendMessage)
  ├─ sylvander-channel-wechat     WeChat enterprise (encrypted XML)
  ├─ sylvander-channel            Channel trait (lightweight contract)
  ├─ sylvander-runtime            bootstrap, session persistence
  ├─ sylvander-agent              engine, AgentRun, bus, tools, memory, approval
  └─ sylvander-llm-anthropic      Anthropic wire protocol
```

## Core capabilities

| Feature | Current status |
|---|---|
| Multi-agent and isolated concurrent sessions | Implemented in the Agent layer |
| Bus message routing and streaming | Implemented |
| Durable session history and usage | Implemented |
| Tool approval and AskUser | Implemented; policy scoping remains in backlog |
| Persistent Agent memory | Not implemented |
| Session-scoped model/workspace overrides | Not implemented |
| AGENTS.md, Skills, and MCP runtime | Not implemented |
| Local/SSH/container/sandbox executors | Not implemented |
| Multi-instance channel supervision | Not implemented |

## Quickstart

```bash
# Configure
export ANTHROPIC_API_KEY=sk-...
export SYLVANDER_MODEL=MiniMax-M3

# Run
cargo run -p sylvander-server --release

# Test
curl -X POST http://localhost:8080/chat \
  -H 'Content-Type: application/json' \
  -d '{"session_id":"test","message":"hello"}'
```

For tool approval, set `SYLVANDER_APPROVAL=1`. Add
`SYLVANDER_APPROVAL_STORE=/path/to/approvals.json` only when durable
exact-request approval is permitted; otherwise the TUI offers one-shot and
session scopes only.

## AskUser tool

The model can call the built-in `ask_user` tool to ask the user a
clarifying question. The loop pauses, publishes a bus event, and
resumes with the answer. Supports:

- Free-text input (no `options`)
- Single choice from a list
- Multi-select (`multi_select: true`)

```json
{"type":"ask_user","call_id":"...","question":"A or B?","options":["A","B"]}
{"type":"answer","call_id":"...","answer":"A"}
```

## Build & Test

```bash
cargo build --workspace
cargo test --workspace
```

## Conventions

- MSRV: 1.96, edition 2024
- Async: tokio (multi-thread)
- All 10 crates, 165 tests

## License

MIT
