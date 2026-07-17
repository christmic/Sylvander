# Module Reference — `sylvander-channel-telegram`

> Telegram bot channel — webhook incoming, `sendMessage` outgoing.
> Source: [`sylvander-channel-telegram/src/lib.rs`](../sylvander-channel-telegram/src/lib.rs)

## 1. Purpose

`sylvander-channel-telegram` integrates Sylvander with the
Telegram Bot API. The channel serves a webhook on a local
`SocketAddr`, validates incoming updates against a configured
`webhook_secret`, and posts agent output via `sendMessage`.

## 2. Setup

```bash
export TELEGRAM_BOT_TOKEN=...
curl -X POST https://api.telegram.org/bot${TOKEN}/setWebhook \
  -d "url=https://your-host/telegram/webhook"
```

The runtime resolves the bot token and the webhook secret through
`SystemSecretResolver`; nothing is read from the environment at
runtime.

## 3. Public surface

```rust
#[derive(Debug, Deserialize)]
pub struct Update { pub update_id: i64, pub message: Option<Message> }
#[derive(Debug, Clone, Deserialize)]
pub struct Message { pub message_id: i64, pub from: Option<User>,
                      pub chat: Chat, pub text: Option<String> }
#[derive(Debug, Clone, Deserialize)]
pub struct User { pub id: i64, pub first_name: String }
#[derive(Debug, Clone, Deserialize)]
pub struct Chat { pub id: i64, #[serde(rename = "type")] pub chat_type: String }

pub struct TelegramChannel { /* see lib.rs */ }
impl TelegramChannel {
    pub fn new(token: impl Into<String>, addr: SocketAddr,
               agent_id: impl Into<AgentId>) -> Self;
    pub fn with_webhook_secret(mut self, secret: impl Into<String>) -> Self;
    pub fn with_instance_id(mut self, instance_id: impl Into<String>) -> Self;
    pub const fn with_request_limit(mut self, max_request_bytes: usize) -> Self;
}
```

## 4. Auth model

Two layers:

- **Telegram → Sylvander**: the webhook validates the `X-Telegram-Bot-Api-Secret-Token`
  header (set by Telegram when `setWebhook` was called with a secret).
- **Sylvander → Telegram**: the bot token authorizes outgoing
  `sendMessage` calls.

The runtime attaches a `BoundaryContext` with
`AuthenticationMethod::WebhookSignature` to inbound requests.

## 5. Lifecycle

1. **Construct** with `TelegramChannel::new(token, addr, agent_id)`.
2. **Configure** with `with_webhook_secret`, `with_instance_id`,
   `with_request_limit`.
3. **Start** — the channel binds the webhook listener.
4. **Receive** — Telegram POSTs an `Update`; the channel maps the
   chat id to a session and submits the chat through the bus.
5. **Send** — the runtime polls the bus for outgoing
   `UiServerMessage` events and posts them via `sendMessage`.
6. **Replay** — duplicate updates are dropped via a `ReplayCache`
   keyed on `update_id`.
7. **Shutdown** — runtime drains in-flight updates and closes the
   listener.

## 6. Tests

Unit tests live in `sylvander-channel-telegram/src/lib.rs`
(`mod tests`); they cover webhook-secret validation, replay cache,
and the `Update` deserialization against sample Telegram payloads.

## 7. Common pitfalls

- Skipping `setWebhook` — without it Telegram cannot deliver
  updates to your server.
- Trusting the `from` field for authorization — Telegram IDs are
  identifiers, not auth. Pair with allowlists in
  `AgentAccessDraft`.
- Sharing one channel across multiple bots — the bot token is
  per-channel, never reuse `instance_id`.

## 8. Related docs

- [`docs/server-configuration.md`](server-configuration.md) — `ChannelTransportConfig::Telegram`.
- [`docs/boundary-authorization.md`](boundary-authorization.md) — `AuthenticationMethod::WebhookSignature`.
- [`docs/chat-channel-operations.md`](chat-channel-operations.md) — operator workflow for chat channels.
- [`AGENTS.md`](../AGENTS.md) — project-wide agent guide.

Co-Authored-By: 🦀 <oraculo@oraculo.ai>