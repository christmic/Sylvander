# Module Reference — `sylvander-channel-http`

> HTTP debug channel — curl-friendly API with SSE streaming.
> Source: [`sylvander-channel-http/src/lib.rs`](../sylvander-channel-http/src/lib.rs)

## 1. Purpose

`sylvander-channel-http` exposes Sylvander over plain HTTP and
Server-Sent Events so operators can drive the agent loop with
`curl`, scripts, or quick automation. It is **not** the public
client protocol (that lives in `sylvander-channel-ws`). Use this
channel for local debug, smoke tests, and CI jobs.

## 2. Protocol summary

- One TCP listener bound to a `SocketAddr`.
- `POST /chat` accepts `{"session_id":"...","message":"..."}` and
  streams events as `event: ...\ndata: <json>\n\n` SSE frames.
- Bearer-token authentication when configured via `with_bearer_auth`.
- Operational health surfaced via `with_operational_health` (see
  `OperationalHealth` struct).

## 3. Public surface

```rust
pub struct HttpChannel { /* see lib.rs */ }
impl HttpChannel {
    pub fn new(addr: SocketAddr, agent_id: impl Into<AgentId>) -> Self;
    pub const fn with_request_limit(mut self, max_request_bytes: usize) -> Self;
    pub fn with_bearer_auth(self, instance_id, principal_id, bearer_token) -> Self;
    pub fn with_operational_health(self, provider: OperationalHealthProvider) -> Self;
}
pub type OperationalHealthProvider =
    Arc<dyn Fn() -> OperationalHealthFuture + Send + Sync + 'static>;
pub struct OperationalHealth {
    pub ready: bool, pub agents: usize,
    pub persistent_sessions: usize, pub ephemeral_sessions: usize,
    pub ready_channels: usize, pub total_channels: usize,
    pub bus_subscribers: usize, pub bus_capacity: usize,
    pub published_messages: u64, pub backpressure_rejections: u64,
}
```

## 4. Auth model

`with_bearer_auth` is **optional**. When omitted, every request is
treated as an internal call without an `AuthenticatedPrincipal`.
When supplied, the channel requires the configured `bearer_token` on
each `POST /chat` and attaches the configured `principal_id` to the
resulting `BoundaryContext`. Tokens are resolved at startup via
`SystemSecretResolver` (see `docs/module-sylvander-server.md`).

## 5. Lifecycle

1. **Construct** with `HttpChannel::new(addr, agent_id)`.
2. **Configure** with `with_request_limit` (default 1 MiB) and
   optionally `with_bearer_auth` / `with_operational_health`.
3. **Start** by handing the channel to `Runtime::start_channels`
   (see `sylvander-server` composition root).
4. **Supervise** — `Runtime` restarts the channel on failure
   according to `ChannelRestartPolicy`.
5. **Shutdown** — runtime drains in-flight SSE responses and closes
   the listener.

## 6. Tests

Unit tests live alongside the source (`sylvander-channel-http/src/lib.rs`
`mod tests`) and verify auth wiring, request-size enforcement, and
the SSE envelope.

## 7. Common pitfalls

- Forgetting `with_request_limit` — the default (1 MiB) is fine for
  short debug messages but easy to forget for large attachments.
- Treating the bearer token as a feature flag — it is the
  authentication boundary; never log the resolved value.
- Reusing the same `instance_id` for two channels — `BoundaryContext`
  requires the configured channel instance id, not the transport
  kind.

## 8. Related docs

- [`docs/server-configuration.md`](server-configuration.md) — `ChannelTransportConfig::Http`.
- [`docs/boundary-authorization.md`](boundary-authorization.md) — `BoundaryContext` and denial codes.
- [`docs/chat-channel-operations.md`](chat-channel-operations.md) — operator workflow for chat channels.
- [`AGENTS.md`](../AGENTS.md) — project-wide agent guide.

Co-Authored-By: 🦀 <oraculo@oraculo.ai>