# Module Reference — `sylvander-channel-ws`

> WebSocket channel — desktop client integration.
> Source: [`sylvander-channel-ws/src/lib.rs`](../sylvander-channel-ws/src/lib.rs)

## 1. Purpose

`sylvander-channel-ws` is a bidirectional transport for Sylvander desktop
clients. It carries the capability-advertised subset of `UiClientMessage` and
`UiServerMessage` over one WebSocket connection, giving full-duplex
interaction with the agent loop.

## 2. Protocol summary

JSON-over-WebSocket. Each frame is a tagged enum value:

- **Client → Server** examples: `chat`, `approve`, `answer`,
  `list_sessions`, `discover_agents`, `select_model`, `submit_feedback`,
  `ping`, ...
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
    pub fn with_bearer_lease(
        self,
        instance_id,
        principal_id,
        source: Arc<dyn CredentialLeaseSource>,
    ) -> Result<Self, CredentialLeaseError>;
}
```

## 4. Auth model

Authentication is bearer-token based, attached via
`with_bearer_lease`. Every upgrade acquires an exact
`bearer_token` lease for the channel instance. Rotation therefore applies to
the next connection without a process restart. Expired, unavailable, or
malformed leases reject the upgrade before any `UiClientMessage` is parsed;
an established socket keeps its already-authenticated principal rather than
re-authenticating individual frames.

## 5. Lifecycle

1. **Construct** with `WsChannel::new(addr, agent_id)`.
2. **Configure** with `with_request_limit` (default 1 MiB) and
   `with_bearer_lease`.
3. **Start** via `Runtime::start_channels`.
4. **Connect** — a client surface opens a WebSocket and sends `Hello` first;
   the server replies with `Welcome` carrying the negotiated protocol version
   and capabilities. One socket may carry multiple explicitly tagged sessions.
5. **Stream** — chat turns stream `text_delta`, `thinking_delta`,
   `tool_call`, `tool_result`, and `iteration_start`, then finish with `done`
   or `error`.
6. **Session discovery** — `list_sessions` dispatches through Runtime
   `UiService`, preserving stable-user visibility rules, and returns one typed
   `sessions_list` response.
7. **Memory confirmation** — when `memory_confirmation_v1` was negotiated,
   list/decide envelopes pass unchanged to Runtime under the authenticated
   WebSocket boundary. The adapter never derives or accepts owner identity.
8. **Shutdown** — runtime closes idle connections gracefully and
   aborts stuck ones on supervisor shutdown.

## 6. Tests

Unit tests in `sylvander-channel-ws/tests/unit/lib.rs` cover the mandatory
handshake, capability negotiation, live bearer rotation and lease failure,
Runtime-owned identity and administration dispatch, redaction, per-session
model changes, Runtime-owned session listing, approval transport, and
request-size limits. Governed-memory confirmation uses the same exhaustive
message dispatcher; its typed shapes, Runtime ownership, and real transport
round trip are covered by the protocol, Runtime, and Unix suites. Add a
WebSocket-specific round-trip case whenever WebSocket framing or dispatch
changes.

## 7. Common pitfalls

- Sending a shape outside the current negotiated protocol contract — the
  channel rejects business messages before `Hello` and returns a typed
  `ProtocolError` for a non-overlapping range. It does not translate obsolete
  envelopes.
- Assuming one socket per session — the channel allows multi-session
  use; tag every outbound message with the `session_id`.
- Treating `auth` as transport-only — it scopes the
  `BoundaryContext.principal` for downstream authorization.

## 8. Related docs

- [`docs/server-configuration.md`](server-configuration.md) — `ChannelTransportConfig::Websocket`.
- [`docs/boundary-authorization.md`](boundary-authorization.md) — `BoundaryContext` and principal scoping.
- [`docs/identity-binding-protocol.md`](identity-binding-protocol.md) — identity-link negotiation over WebSocket.
- [`docs/credential-leases.md`](credential-leases.md) — renewable credential contract.
- [`AGENTS.md`](../AGENTS.md) — project-wide agent guide.

Co-Authored-By: 🦀 <oraculo@oraculo.ai>
