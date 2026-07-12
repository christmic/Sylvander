# Sylvander v2

AI Agent framework in Rust — multi-agent, multi-session, multi-channel,
with structured tool approval and user clarification.

## Architecture

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

| Feature | Status |
|---|---|
| Multi-agent, multi-session (N:N) | M4-M6 |
| Bus-based message routing (chat/stream/system) | M4-M8 |
| Memory: read tool exposed, write system-driven | M8 |
| Tool approval (rule-based + bus) | M12 |
| Streaming events (text/thinking/tool/iteration/done) | M11 |
| AskUser tool (model asks mid-loop) | M18 |
| Channels: HTTP/WS/Unix/DingTalk/Telegram/WeChat | M13-M17 |
| Approval + AskUser over WebSocket | M18 |

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
