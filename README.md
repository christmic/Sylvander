# Sylvander v2

AI Agent framework in Rust — multi-agent, multi-session, multi-channel,
with structured tool approval and user clarification.

For an implementation-oriented map of every Rust module, start at the
[documentation index](docs/INDEX.md). The detailed Agent, Runtime, common
Channel, provider, and TUI designs live beside their owning crates and are
linked there.

## Architecture

The normative server-Agent architecture, implementation audit, and ordered
production backlog live in
[`docs/sylvander-agent-platform.md`](docs/sylvander-agent-platform.md). Crate
presence does not by itself mean that an adapter is wired into the production
server; the audit records that distinction explicitly.
Deployment configuration is documented in
[`docs/server-configuration.md`](docs/server-configuration.md).
Runtime evidence and the self-improvement safety boundary are documented in
[`docs/runtime-evidence.md`](docs/runtime-evidence.md).
Ingress authentication, authorization, limits, and denial audit are documented
in [`docs/boundary-authorization.md`](docs/boundary-authorization.md).

```
sylvander-server                  binary — boots the system
  └─ sylvander-runtime             trusted composition, persistence, supervision
       ├─ sylvander-agent           run engine, prompts, tools, memory, workspaces
       │   ├─ sylvander-llm-core    provider-neutral model contract
       │   ├─ sylvander-llm-anthropic
       │   └─ sylvander-protocol    cross-language IDs and wire envelopes
       └─ sylvander-channel         authenticated ingress boundary
           ├─ channel-unix          local TUI protocol
           ├─ channel-http / channel-ws
           └─ channel-dingtalk / channel-telegram / channel-wechat

sylvander-tui                      standalone single-session terminal client
```

## Core capabilities

| Feature | Current status |
|---|---|
| Isolated concurrent sessions and streamed bus routing | Implemented |
| Durable transcript, usage, relationship memory, and evidence | Implemented |
| Tool approval, AskUser, plans, and bounded background tasks | Implemented |
| Agent defaults plus session-scoped model/workspace overrides | Implemented |
| `AGENTS.md`, Skills, and supervised MCP stdio | Implemented |
| Local executor, isolated Git worktree, OCI container/sandbox policy | Implemented |
| Multi-instance channel supervision | Implemented for current adapters |
| OpenSSH executor and remote Git worktrees | Implemented; each deployment must pass the opt-in real-SSH acceptance journey |
| Native SSH terminal and native tmux integration | Not part of the current client contract |

## Quickstart

Self-use mode is the supported local, single-user startup profile. It keeps
sessions and Agent memory in SQLite, allows the authenticated local Unix user
to use the default Agent, and does not require a separately administered
memory-integrity anchor.

```bash
# Configure the provider, qualified model, Agent, channel, and self-use mode.
cp config/sylvander.example.toml /tmp/sylvander.toml
$EDITOR /tmp/sylvander.toml
export SYLVANDER_CONFIG=/tmp/sylvander.toml

# Export every secret named by that document.
export ANTHROPIC_API_KEY=...
export SYLVANDER_MEMORY_INTEGRITY_KEY=...
export SYLVANDER_EVIDENCE_ENCRYPTION_KEY=...

# Start the server and TUI in separate terminals.
cargo run -p sylvander-server --release
cargo run -p sylvander-tui --release -- /tmp/sylvander.sock
```

For self-use, set `server.mode = "self_use"` in that document. Production uses
`server.mode = "production"` and requires both a memory-integrity key and an
independent file or HTTP anchor; see
[`docs/server-configuration.md`](docs/server-configuration.md).

Tool approval is configured under `[server.approval]`. Set `enabled = true`;
add `persistent_store` only when durable exact-request approval is permitted.
Otherwise the TUI offers one-shot and session scopes only.

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
- The workspace is continuously verified by its Rust test, Clippy, format,
  security, performance, clean-room, and pseudo-terminal gates; see
  [`docs/release-closure.md`](docs/release-closure.md) for reproducible
  evidence rather than a stale test count.

## License

MIT
