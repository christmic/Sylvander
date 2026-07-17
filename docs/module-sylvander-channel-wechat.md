# Module Reference — `sylvander-channel-wechat`

> WeChat enterprise bot channel — encrypted XML callbacks.
> Source: [`sylvander-channel-wechat/src/lib.rs`](../sylvander-channel-wechat/src/lib.rs)

## 1. Purpose

`sylvander-channel-wechat` integrates Sylvander with WeChat Work
(enterprise bot). Inbound callbacks are AES-encrypted XML
payloads verified with the configured `token` /
`encoding_aes_key`. Outbound messages are routed through the bus
to the WeChat reply path.

## 2. Architecture

```text
lib.rs       — Channel trait impl + axum webhook server
                |
                v
protocol.rs  — WeChat crypto (parse_message_xml, WechatCrypto)
                pure SDK, no Sylvander deps
```

## 3. Public surface

```rust
pub mod protocol;
pub struct WechatChannel { /* see lib.rs */ }
impl WechatChannel {
    pub fn new(token: String, encoding_aes_key: String, corp_id: String,
               webhook_addr: SocketAddr,
               agent_id: impl Into<AgentId>) -> Result<Self, String>;
    pub fn with_instance_id(mut self, instance_id: impl Into<String>) -> Self;
    pub const fn with_request_limit(mut self, max_request_bytes: usize) -> Self;
}
```

## 4. Auth model

WeChat signs every callback with an HMAC computed from the
configured `token`. The channel also AES-decrypts the payload
using `encoding_aes_key` and verifies the recipient `corp_id`. A
failing signature or decryption returns a 401-style denial and
never reaches the bus.

The runtime attaches `AuthenticationMethod::WebhookSignature` to
the resulting `BoundaryContext`.

## 5. Lifecycle

1. **Construct** with `WechatChannel::new(token, encoding_aes_key,
   corp_id, addr, agent_id)`. The constructor initializes the
   crypto via `WechatCrypto::new` and fails fast on a malformed
   `encoding_aes_key`.
2. **Configure** with `with_instance_id` and `with_request_limit`.
3. **Start** — the channel binds the axum webhook listener and
   exposes `GET /` (URL verification) and `POST /` (callback).
4. **Receive** — callbacks are decrypted, parsed, mapped to a
   session via `instance_id`, and submitted through the bus.
5. **Replay** — duplicate message ids are dropped via a
   `ReplayCache` to defend against WeChat retries.
6. **Shutdown** — runtime drains in-flight callbacks and unbinds
   the listener.

## 6. Tests

- `sylvander-channel-wechat/src/lib.rs` (`mod tests`) — webhook
  handlers, replay cache, instance-id binding.
- `sylvander-channel-wechat/src/protocol.rs` (`mod tests`) — pure
  crypto tests against the WeChat reference vectors.

## 7. Common pitfalls

- Mixing up `token` and `encoding_aes_key` — both are required;
  `token` signs requests, `encoding_aes_key` decrypts them.
- Sharing `corp_id` across multiple channels — every bot is
  scoped to one corp.
- Ignoring the URL verification handshake — WeChat requires the
  `GET /` echo before it will deliver real callbacks.
- Forgetting `with_instance_id` — without it the channel cannot
  bind sessions to a configured agent.

## 8. Related docs

- [`docs/server-configuration.md`](server-configuration.md) — `ChannelTransportConfig::Wechat`.
- [`docs/boundary-authorization.md`](boundary-authorization.md) — `AuthenticationMethod::WebhookSignature`.
- [`docs/chat-channel-operations.md`](chat-channel-operations.md) — operator workflow for chat channels.
- [`AGENTS.md`](../AGENTS.md) — project-wide agent guide.

Co-Authored-By: 🦀 <oraculo@oraculo.ai>