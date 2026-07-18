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
- Renewable bearer authentication configured via `with_bearer_lease`.
- Operational health surfaced via `with_operational_health` (see
  `OperationalHealth` struct).

## 3. Public surface

```rust
pub struct HttpChannel { /* see lib.rs */ }
impl HttpChannel {
    pub fn new(addr: SocketAddr, agent_id: impl Into<AgentId>) -> Self;
    pub const fn with_request_limit(mut self, max_request_bytes: usize) -> Self;
    pub fn with_bearer_lease(
        self,
        instance_id,
        principal_id,
        source: Arc<dyn CredentialLeaseSource>,
    ) -> Result<Self, CredentialLeaseError>;
    pub fn with_operational_health(self, provider: OperationalHealthProvider) -> Self;
}
pub type OperationalHealthProvider =
    Arc<dyn Fn() -> OperationalHealthFuture + Send + Sync + 'static>;
pub struct OperationalHealth {
    pub ready: bool, pub agents: usize,
    pub persistent_sessions: usize,
    pub ready_channels: usize, pub total_channels: usize,
    pub bus_subscribers: usize, pub bus_capacity: usize,
    pub published_messages: u64, pub backpressure_rejections: u64,
}
```

## 4. Auth model

Every `POST /chat` acquires an exact `bearer_token` lease for the configured
channel instance. The server composition root supplies the `instance_id`,
`principal_id`, and Runtime-owned lease source; the channel never receives the
configured secret locator. Rotation is visible to the next request without a
restart. Lease failure, expiry, or a malformed slot set rejects the request
before chat parsing. Successful authentication attaches that principal to the
resulting `BoundaryContext`; health, readiness, and metrics remain separate
operational endpoints.

## 5. Lifecycle

1. **Construct** with `HttpChannel::new(addr, agent_id)`.
2. **Configure** with `with_request_limit` (default 1 MiB),
   `with_bearer_lease`, and optionally `with_operational_health`.
3. **Start** by handing the channel to `Runtime::start_channels`
   (see `sylvander-server` composition root).
4. **Supervise** — `Runtime` restarts the channel on failure
   according to `ChannelRestartPolicy`.
5. **Shutdown** — runtime drains in-flight SSE responses and closes
   the listener.

## 6. Tests

White-box unit tests live in
`sylvander-channel-http/tests/unit/lib.rs`, linked by the production module's
test-only bridge. They verify auth wiring, request-size enforcement, and the
  Runtime authorization boundary, live bearer rotation and fail-closed lease
  errors, and operational health/readiness/metrics without keeping test bodies
  under `src/`.

## 7. Common pitfalls

- Forgetting `with_request_limit` — the default (1 MiB) is fine for
  short debug messages but easy to forget for large attachments.
- Treating the bearer token as a feature flag — it is the
  authentication boundary; never log the resolved value.
- Constructing a channel without bearer identity — `/chat` remains
  unauthenticated and returns a denial; there is no trusted-development
  bypass.
- Reusing the same `instance_id` for two channels — `BoundaryContext`
  requires the configured channel instance id, not the transport
  kind.

## 8. Related docs

- [`docs/server-configuration.md`](server-configuration.md) — `ChannelTransportConfig::Http`.
- [`docs/boundary-authorization.md`](boundary-authorization.md) — `BoundaryContext` and denial codes.
- [`docs/chat-channel-operations.md`](chat-channel-operations.md) — operator workflow for chat channels.
- [`docs/credential-leases.md`](credential-leases.md) — renewable credential contract.
- [`AGENTS.md`](../AGENTS.md) — project-wide agent guide.

Co-Authored-By: 🦀 <oraculo@oraculo.ai>
