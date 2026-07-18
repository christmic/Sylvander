# Module Reference — `sylvander-channel-unix`

> Unix domain socket channel — line-based JSON protocol.
> Source: [`sylvander-channel-unix/src/lib.rs`](../sylvander-channel-unix/src/lib.rs)

## 1. Purpose

`sylvander-channel-unix` exposes Sylvander over a Unix domain socket
so the host TUI and other local tools can talk to the server
without TCP, TLS, or bearer tokens. It is the lowest-friction
transport for local development.

## 2. Protocol summary

One JSON object per line (`tokio_util::codec::LinesCodec`).
Commands flow client → server; events flow server → client. Wire
shapes are the same `UiClientMessage` / `UiServerMessage` enums
used by the WebSocket channel.

## 3. Public surface

```rust
pub struct UnixChannel { /* see lib.rs */ }
#[derive(Clone)]
pub struct RuntimeInfo {
    pub model: ModelSelection,
    pub reasoning_effort: ReasoningEffort,
    pub models: Vec<ModelDescriptor>,
    pub permissions: PermissionProfile,
    pub capabilities: u8,
    pub approval_enabled: bool,
    pub max_attachment_bytes: usize,
    pub platform: PlatformSnapshot,
    pub platform_provider: Option<Arc<dyn Fn() -> PlatformSnapshot + Send + Sync>>,
}
impl UnixChannel {
    pub fn new(path: impl Into<PathBuf>, agent_id: impl Into<AgentId>) -> Self;
    pub fn with_instance_id(mut self, instance_id: impl Into<String>) -> Self;
    pub const fn with_request_limit(mut self, max_request_bytes: usize) -> Self;
    pub fn with_runtime_info(mut self, info: RuntimeInfo) -> Self;
}
```

## 4. Auth model

Authentication uses two operating-system guarantees: the socket is created
owner-only (`0o600`), and every accepted connection must yield peer credentials.
The peer UID becomes a transport-scoped principal using
`AuthenticationMethod::UnixPeer`. A peer-credential lookup failure is reported
through Runtime's authentication-denial path and the connection is discarded;
there is no bearer token or anonymous local fallback. The resulting
`BoundaryContext` carries the configured `instance_id`, `unix` transport, and
authenticated principal.

## 5. Lifecycle

1. **Construct** with `UnixChannel::new(socket_path, agent_id)`.
2. **Configure** with `with_instance_id`,
   `with_request_limit`, and `with_runtime_info` (the server
   composition root supplies a fully-populated `RuntimeInfo`).
3. **Start** — the channel creates the socket, applies mode `0o600`,
   and begins accepting line-delimited clients.
4. **Handshake** — the first frame must negotiate the current UI protocol
   range; business messages sent before `Hello` are rejected.
5. **Replay** — disconnected clients can request `LoadSession` and
   receive a buffered `session_history` (with optional truncation).
6. **Shutdown** — runtime removes the socket path on stop.

## 6. Tests

Unit tests live in `sylvander-channel-unix/tests/unit/lib.rs`,
covering framing bounds, protocol negotiation, peer/session isolation,
Runtime-owned administration and identity dispatch, redaction, session
lifecycle, replay, attachments, approvals, tasks, and socket permissions.

## 7. Common pitfalls

- Leaving a stale socket file — the channel always recreates the
  socket, but operators may need to clean up after a hard kill.
- Forgetting `with_runtime_info` — the server composition root
  wires this from the agent descriptor; in tests you must call it
  yourself.
- Sending multiple JSON objects on one line — `LinesCodec` is
  strict and will reject the frame.

## 8. Related docs

- [`docs/server-configuration.md`](server-configuration.md) — `ChannelTransportConfig::Unix`.
- [`docs/boundary-authorization.md`](boundary-authorization.md) — `AuthenticationMethod::UnixPeer`.
- [`docs/chat-channel-operations.md`](chat-channel-operations.md) — operator workflow for chat channels.
- [`AGENTS.md`](../AGENTS.md) — project-wide agent guide.

Co-Authored-By: 🦀 <oraculo@oraculo.ai>
