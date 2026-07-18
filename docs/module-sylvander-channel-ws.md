# Module Reference — `sylvander-channel-ws`

> WebSocket channel — desktop client integration.
> Source: [`sylvander-channel-ws/src/lib.rs`](../sylvander-channel-ws/src/lib.rs)

## 1. Purpose

`sylvander-channel-ws` is the primary bidirectional transport for
Sylvander desktop clients. It carries the full `UiClientMessage`
and `UiServerMessage` envelope over a single WebSocket connection,
giving full-duplex interaction with the agent loop.

## 2. Protocol summary

JSON-over-WebSocket. Each frame is a tagged enum value:

- **Client → Server** examples: `chat`, `approve`, `answer`,
  `interrupt`, `list_sessions`, `discover_agents`, `select_model`,
  `submit_feedback`, `ping`, ...
- **Server → Client** examples: `session_created`, `text_delta`,
  `thinking_delta`, `tool_call`, `tool_result`, `iteration_start`,
  `iteration_end`, `done`, `approval_request`, `error`, `pong`, ...

The complete enum list lives in `sylvander-protocol/src/ui.rs`
(`UiClientMessage` / `UiServerMessage`).

## 3. Public surface

```rust
pub struct WsChannel { /* see lib.rs */ }
impl WsChannel {
    pub fn new(addr: SocketAddr, agent_id: impl Into<AgentId>) -> Self;
    pub const fn with_request_limit(mut self, max_request_bytes: usize) -> Self;
    pub fn with_bearer_auth(self, instance_id, principal_id, bearer_token) -> Self;
}
```

## 4. Auth model

Authentication is bearer-token based, attached via
`with_bearer_auth`. The resolved token is required on every
upgrade; rejected upgrades close the socket before any
`UiClientMessage` is parsed. Tokens are resolved at startup via
`SystemSecretResolver`.

## 5. Lifecycle

1. **Construct** with `WsChannel::new(addr, agent_id)`.
2. **Configure** with `with_request_limit` (default 1 MiB) and
   optionally `with_bearer_auth`.
3. **Start** via `Runtime::start_channels`.
4. **Connect** — clients open one WebSocket per session and send
   `Hello` first; the server replies with `Welcome` carrying the
   negotiated protocol version and capabilities.
5. **Stream** — chat turns stream `text_delta`, `tool_call`,
   `tool_result`, `iteration_*`, and finish with `done` or `error`.
6. **Shutdown** — runtime closes idle connections gracefully and
   aborts stuck ones on supervisor shutdown.

## 6. Tests

Unit tests in `sylvander-channel-ws/tests/unit/lib.rs` cover
frame parsing, auth enforcement, and request-size limits.

## 7. Common pitfalls

- Using the legacy client shapes against a v3 server — the channel
  negotiates a `UiProtocolHello` range, so wire mismatches should
  surface as a `ProtocolError`.
- Assuming one socket per session — the channel allows multi-session
  use; tag every outbound message with the `session_id`.
- Treating `auth` as transport-only — it scopes the
  `BoundaryContext.principal` for downstream authorization.

## 8. Related docs

- [`docs/server-configuration.md`](server-configuration.md) — `ChannelTransportConfig::Websocket`.
- [`docs/boundary-authorization.md`](boundary-authorization.md) — `BoundaryContext` and principal scoping.
- [`docs/identity-binding-protocol.md`](identity-binding-protocol.md) — identity-link negotiation over WebSocket.
- [`AGENTS.md`](../AGENTS.md) — project-wide agent guide.

Co-Authored-By: 🦀 <oraculo@oraculo.ai>
