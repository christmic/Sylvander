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
lib.rs      — Channel trait impl (glue: session mapping, bus pub/sub)
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
pub struct DingTalkIncoming { pub callback: RobotMessage }
pub struct DingTalkOutgoing { pub kind: String, pub text: String }
pub type DingTalkTextContent = protocol::TextContent;

pub struct DingTalkChannel { /* see lib.rs */ }
impl DingTalkChannel {
    pub fn new(app_key: impl Into<String>, app_secret: impl Into<String>) -> Self;
    pub fn with_identity(self, instance_id, agent_id) -> Self;
    pub const fn with_request_limit(mut self, max_request_bytes: usize) -> Self;
}
```

## 4. Auth model

The channel authenticates with DingTalk using an `app_key` /
`app_secret` pair. The runtime resolves both through
`SystemSecretResolver` and constructs the stream `Client`. There is
no per-message credential; instead, the runtime attaches a
`BoundaryContext` with `AuthenticationMethod::PlatformIdentity`
once the stream is established.

## 5. Lifecycle

1. **Construct** with `DingTalkChannel::new(app_key, app_secret)`.
2. **Configure** with `with_identity` and `with_request_limit`.
3. **Stream open** — the channel opens the DingTalk stream and
   subscribes to `ROBOT_TOPIC`.
4. **Message handling** — `ChannelMessageHandler::on_message`
   dedupes by `msg_id`, maps the conversation to a session, and
   submits the chat through the bus.
5. **Restart** — failures bubble to `Runtime` and trigger the
   configured `ChannelRestartPolicy` backoff.
6. **Shutdown** — runtime closes the stream and waits for any
   in-flight message to complete.

## 6. Tests

- `sylvander-channel-dingtalk/tests/unit/lib.rs` — session
  mapping, replay cache, message routing.
- `sylvander-channel-dingtalk/tests/unit/protocol.rs` — pure
  SDK tests for stream frames and signature verification.

## 7. Common pitfalls

- Re-using the same `instance_id` across channels — every DingTalk
  bot has its own conversation namespace; collisions silently
  merge sessions.
- Forgetting `with_identity` — without it the channel cannot bind
  to a configured agent and the runtime rejects startup.
- Sending raw text — DingTalk replies go through `Outgoing` with
  the configured `kind`; never call the DingTalk API directly.

## 8. Related docs

- [`docs/server-configuration.md`](server-configuration.md) — `ChannelTransportConfig::DingTalk`.
- [`docs/boundary-authorization.md`](boundary-authorization.md) — `AuthenticationMethod::PlatformIdentity`.
- [`docs/chat-channel-operations.md`](chat-channel-operations.md) — operator workflow for chat channels.
- [`AGENTS.md`](../AGENTS.md) — project-wide agent guide.

Co-Authored-By: 🦀 <oraculo@oraculo.ai>
