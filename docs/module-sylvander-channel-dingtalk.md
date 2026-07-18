# Module Reference — `sylvander-channel-dingtalk`

> DingTalk bot channel — DingTalk Stream callback ingestion.
> Source: [`sylvander-channel-dingtalk/src/lib.rs`](../sylvander-channel-dingtalk/src/lib.rs)

## 1. Purpose

`sylvander-channel-dingtalk` integrates Sylvander with DingTalk's
enterprise bot platform. It implements Sylvander's `Channel`
trait on top of the DingTalk Stream callback protocol (provided by
the local `protocol` module).

## 2. Architecture

```text
lib.rs      — Channel trait impl (authenticated ingress + outbound subscription)
              |
              v
protocol.rs — DingTalk Stream protocol (pure SDK, no Sylvander deps)
              Client, RobotMessage, MessageHandler
```

The `lib.rs` layer is the only place that knows about Sylvander's
agent types; `protocol.rs` is a vendor-pure SDK with its own
test suite.

## 3. Public surface

```rust
pub mod protocol;
pub use protocol::{FrameHeaders, MessageHandler, ROBOT_TOPIC, RobotMessage};
pub use protocol::Client as DingTalkClient;
pub type DingTalkCallback = RobotMessage;
pub type DingTalkTextContent = protocol::TextContent;

pub struct DingTalkChannel { /* see lib.rs */ }
impl DingTalkChannel {
    pub fn new(
        instance_id,
        agent_id,
        credentials: Arc<dyn CredentialLeaseSource>,
    ) -> Result<Self, CredentialLeaseError>;
    pub fn with_request_limit(mut self, max_request_bytes: usize) -> Self;
}
```

## 4. Auth model

The channel authenticates with DingTalk using one atomic
`app_key`/`app_secret` lease scoped to `instance_id`. It acquires the bundle
when opening a Stream connection and before access-token cache use. The cache
is bound to the bundle's credential generation, so rotation invalidates an
otherwise unexpired DingTalk access token. Lease acquisition or renewal
failure closes or rejects the current operation without reusing old app
credentials. Accepted callbacks use
`AuthenticationMethod::PlatformIdentity`.

## 5. Lifecycle

1. **Construct** with `DingTalkChannel::new(instance_id, agent_id,
   credential_source)`.
2. **Configure** with `with_request_limit`.
3. **Stream open** — the channel opens the DingTalk stream, subscribes to
   `ROBOT_TOPIC`, and reports Runtime readiness only after the WebSocket is
   established.
4. **Message handling** — `ChannelMessageHandler::on_message`
   dedupes by `msg_id`, derives a platform principal, maps the conversation to
   an existing session when present, and submits through Runtime-owned
   authenticated ingress.
5. **Restart** — failures bubble to `Runtime` and trigger the
   configured `ChannelRestartPolicy` backoff.
6. **Shutdown** — runtime closes the stream and waits for any
   in-flight message to complete.

## 6. Tests

- `sylvander-channel-dingtalk/tests/unit/lib.rs` — session
  lookup isolation, principal derivation, replay bounds, and request limits.
- `sylvander-channel-dingtalk/tests/unit/protocol.rs` — retry behavior for
  retryable session-webhook delivery and generation-bound access-token cache.

## 7. Common pitfalls

- Re-using the same `instance_id` across channels — every DingTalk
  bot has its own conversation namespace; collisions silently
  merge sessions.
- Requesting `app_key` and `app_secret` separately — DingTalk requires one
  atomic lease so a rotation cannot mix generations.
- Bypassing `DingTalkClient::reply_text` / `reply_markdown` — replies must use
  the session webhook recorded for the same channel instance so credential
  renewal, delivery retries, and instance isolation remain intact.

## 8. Related docs

- [`docs/server-configuration.md`](server-configuration.md) — `ChannelTransportConfig::DingTalk`.
- [`docs/boundary-authorization.md`](boundary-authorization.md) — `AuthenticationMethod::PlatformIdentity`.
- [`docs/chat-channel-operations.md`](chat-channel-operations.md) — operator workflow for chat channels.
- [`docs/credential-leases.md`](credential-leases.md) — renewable credential contract.
- [`AGENTS.md`](../AGENTS.md) — project-wide agent guide.

Co-Authored-By: 🦀 <oraculo@oraculo.ai>
