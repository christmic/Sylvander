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

The composition root registers the bot token and webhook secret as named
secret references. The running channel acquires them as one renewable bundle
at each inbound-authentication or outbound-delivery boundary.

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
    pub fn new(
        addr: SocketAddr,
        agent_id: impl Into<AgentId>,
        instance_id,
        credentials: Arc<dyn CredentialLeaseSource>,
    ) -> Result<Self, CredentialLeaseError>;
    pub const fn with_request_limit(mut self, max_request_bytes: usize) -> Self;
}
```

## 4. Auth model

Two layers:

- **Telegram → Sylvander**: the webhook validates the `X-Telegram-Bot-Api-Secret-Token`
  header (set by Telegram when `setWebhook` was called with a secret).
- **Sylvander → Telegram**: the bot token authorizes outgoing
  `sendMessage` calls.

Both values are returned in one exact-slot lease. The webhook reads the
current secret for every update; outbound delivery reads the current token for
every message chunk. Rotation therefore needs no listener restart. Lease
failure or expiry rejects the operation rather than retaining the prior
value.

Webhook-secret failures enter Runtime's denial path as
`AuthenticationMethod::WebhookSignature`. After the webhook itself is
authenticated, the stable bot-instance/chat pair becomes a transport-scoped
`PlatformIdentity` principal for the accepted request; Telegram display names
never establish identity.

## 5. Lifecycle

1. **Construct** with `TelegramChannel::new(addr, agent_id, instance_id,
   credential_source)`.
2. **Configure** with `with_request_limit`.
3. **Start** — the channel binds the webhook listener.
4. **Receive** — Telegram POSTs an `Update`; the channel maps the
   chat id to authenticated boundary context and submits through Runtime-owned
   ingress. It cannot publish a raw join/chat message.
5. **Send** — the channel subscribes to Runtime stream events and posts
   bounded status messages plus the terminal `Done` text via `sendMessage`.
   Token-by-token text/thinking deltas are deliberately suppressed because
   this adapter does not edit one in-place Telegram message.
6. **Replay** — duplicate updates are dropped via a `ReplayCache`
   keyed on `update_id`.
7. **Shutdown** — runtime drains in-flight updates and closes the
   listener.

## 6. Tests

White-box unit tests live in
`sylvander-channel-telegram/tests/unit/lib.rs`, linked by the production
module's test-only bridge. They cover webhook-secret validation, replay cache,
principal isolation, Unicode-safe response splitting, bounded retry, live
credential rotation, lease failure, and request limits.

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
- [`docs/credential-leases.md`](credential-leases.md) — renewable credential contract.
- [`AGENTS.md`](../AGENTS.md) — project-wide agent guide.

Co-Authored-By: 🦀 <oraculo@oraculo.ai>
