# Module Reference — `sylvander-channel-wechat`

> WeChat enterprise bot channel — encrypted XML callbacks.
> Source: [`sylvander-channel-wechat/src/lib.rs`](../sylvander-channel-wechat/src/lib.rs)

## 1. Purpose

`sylvander-channel-wechat` integrates Sylvander with WeChat Work
(enterprise application). Inbound callbacks are AES-encrypted XML payloads
verified with a renewable `callback_token` / `encoding_aes_key` lease.
Completed replies, tool status, and interactive-control acknowledgements use
the active WeChat Work message API; text deltas are not sent as duplicate
messages.

## 2. Architecture

```text
lib.rs       — Channel lifecycle + webhook ingress + active API delivery
                |                         |
                v                         v
protocol.rs  — callback crypto      CredentialLeaseSource
                and XML parsing       (three named secret slots)
```

## 3. Public surface

```rust
pub mod protocol;
pub struct WechatChannel { /* see lib.rs */ }
impl WechatChannel {
    pub fn new(corp_id: String, wechat_agent_id: String,
               webhook_addr: SocketAddr, agent_id: impl Into<AgentId>,
               instance_id: impl Into<String>,
               credentials: Arc<dyn CredentialLeaseSource>)
               -> Result<Self, String>;
    pub const fn with_request_limit(mut self, max_request_bytes: usize) -> Self;
}
```

## 4. Auth model

WeChat signs every callback with its documented SHA-1 construction: the
channel lexically sorts `token`, `timestamp`, `nonce`, and encrypted payload,
concatenates them, and compares the digest to `msg_signature`. This is not an
HMAC. The channel then AES-256-CBC-decrypts the payload and compares the
embedded recipient `corp_id` with the configured enterprise before returning
message content. The callback codec is rebuilt from a bounded lease for every
request and clears copied token/key bytes on drop. Signature, decryption,
recipient-binding, lease-expiry, and lease-renewal failures enter Runtime's
content-safe authentication-denial path.

The runtime attaches `AuthenticationMethod::WebhookSignature` to
the resulting `BoundaryContext`.

## 5. Lifecycle

1. **Construct** with the enterprise id, numeric WeChat application id,
   Sylvander Agent, stable channel-instance id, and Runtime-owned credential
   source.
2. **Lease** the callback pair atomically from `callback_token` and
   `encoding_aes_key`; lease `api_secret` independently at each outbound
   operation. Startup preflights both paths before readiness.
3. **Start** — the channel binds the axum webhook listener and
   exposes `GET /wechat/callback` (URL verification) and
   `POST /wechat/callback` (callback).
4. **Receive** — callbacks are decrypted, parsed, mapped to a
   session via `instance_id`, and submitted through Runtime-owned
   authenticated ingress.
5. **Replay** — duplicate message ids are dropped via a
   `ReplayCache` to defend against WeChat retries.
6. **Deliver** — acquire/refresh the access token, send a bounded UTF-8 text
   payload, retry throttling/transient failures, and refresh once when WeChat
   rejects an expired token. Credential-generation changes invalidate the
   cached access token immediately.
7. **Control** — `/approve`, `/deny`, `/answer`, and `/interrupt` reuse the
   authenticated session boundary.
8. **Shutdown** — Runtime drains in-flight callbacks and unbinds
   the listener.

## 6. Tests

- `sylvander-channel-wechat/tests/unit/lib.rs` — webhook
  denial routing, replay bounds, principal isolation, request limits, and
  Unicode-safe truncation; hermetic active-API tests cover token reuse,
  credential rotation, invalid-token refresh, and final-message delivery.
- `sylvander-channel-wechat/tests/unit/protocol.rs` — pure
  signature construction, AES-CBC round-trip, cross-enterprise recipient
  rejection, and XML entity parsing.

## 7. Common pitfalls

- Mixing up the three credential slots: `callback_token` signs callbacks,
  `encoding_aes_key` decrypts them, and `api_secret` obtains the outbound
  access token.
- Sharing `corp_id` across multiple channels — every bot is
  scoped to one corp.
- Ignoring the URL verification handshake — WeChat requires the
  `GET /` echo before it will deliver real callbacks.
- Reusing one credential source across instance ids — the lease source rejects
  mismatched instance ids and never falls back to another application's
  credentials.

## 8. Related docs

- [`docs/server-configuration.md`](server-configuration.md) — `ChannelTransportConfig::Wechat`.
- [`docs/boundary-authorization.md`](boundary-authorization.md) — `AuthenticationMethod::WebhookSignature`.
- [`docs/chat-channel-operations.md`](chat-channel-operations.md) — operator workflow for chat channels.
- [`docs/credential-leases.md`](credential-leases.md) — renewable credential contract.
- [`AGENTS.md`](../AGENTS.md) — project-wide agent guide.

Co-Authored-By: 🦀 <oraculo@oraculo.ai>
