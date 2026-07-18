# Boundary authentication and authorization

This document is the normative security contract for requests entering a
Sylvander runtime. A transport authenticates a caller, constructs a
protocol-owned `BoundaryContext`, and asks the runtime to authorize the complete
operation before dispatch. Adapters must not create sessions or publish user
messages around this boundary.

## Trust model

Every request carries four non-secret facts:

- a stable principal ID and principal kind;
- the authentication method that vouched for it;
- the configured channel instance ID;
- a request ID used for correlation and denial audit.

Credentials are resolved from `SecretRef` values and are never retained in the
boundary context. Missing authentication fails closed. A configured Agent also
fails closed unless its access policy explicitly allows the principal, one of
its roles, all authenticated principals, or an internal system principal.

| Transport | Authentication | Principal and instance scope |
|---|---|---|
| Unix | operating-system peer credentials | peer UID under the configured Unix channel ID |
| HTTP | constant-time bearer-token comparison | configured principal under the configured HTTP channel ID |
| WebSocket | bearer token during HTTP upgrade | configured principal under the configured WebSocket channel ID |
| DingTalk | authenticated Stream connection and platform sender identity | `dingtalk:{instance}:{sender}` |
| Telegram | required webhook secret and platform chat identity | `telegram:{instance}:{chat}` |
| WeChat | callback signature plus encrypted payload | `wechat:{instance}:{sender}` |

Telegram rejects every webhook until a non-empty webhook secret is configured.
The production server exposes it through the channel's renewable
`CredentialLeaseSource`; direct library embedding must supply the exact
`bot_token`/`webhook_secret` bundle. There is no static-secret setter or
unauthenticated fallback.

## Authorization invariants

The runtime enforces these rules before work reaches an Agent:

1. every operation requires an authenticated principal;
2. Agent discovery and session creation apply `agents[].access`;
3. a session belongs to the principal recorded in
   `SessionMetadata.user_id`;
4. only that principal, an explicit `admin` role, or an internal system
   principal can read, mutate, control, delete, fork, or submit feedback for
   the session;
5. external identity keys include the configured channel instance, so equal
   chat or user IDs from different bots cannot collide;
6. outbound platform messages verify the same instance binding before using a
   chat ID or webhook;
7. authorization applies to approval, answer, interrupt, plan, task, rollback,
   and configuration operations, not only chat.
8. provider-qualified model and permission selection require a session identity
   and the expected configuration revision; unscoped, bare-model, stale, or
   unavailable selections fail before mutation.

Platform adapters use `authorize_external_chat`. It creates new sessions
through the runtime application service, preserving effective Agent/model/
workspace configuration, then authorizes the actual chat payload. If the
runtime authorizer is absent, the adapter fails closed.

## Limits and public errors

`server.boundary.max_request_bytes` defaults to 1 MiB and accepts values from
1 KiB through 16 MiB. `server.boundary.requests_per_minute` defaults to 240 and
accepts values from 1 through 100,000. The fixed-window rate key combines the
channel instance and principal. The window is intentionally process-local and
resets on restart. Deployments that require a durable or fleet-wide quota must
enforce that additional boundary in their authenticated reverse proxy; the
Runtime does not claim a distributed rate-limit store.

Platform replay caches are isolated per channel instance, bounded to 4096
message IDs, and expire entries after ten minutes. This prevents ordinary
provider retries from executing the same inbound message twice while the
instance is running. The cache is not persisted across process restart; a
deployment requiring a longer idempotency horizon must provide it at the
platform/webhook boundary.

Clients receive a typed `BoundaryDenied` response with one of:

- `unauthenticated`;
- `forbidden`;
- `invalid_scope`;
- `payload_too_large`;
- `rate_limited`, optionally with `retry_after_ms`.

Messages do not reveal credentials, allowlists, or sensitive resource data.
HTTP maps these categories to the corresponding 401, 403, 400, 413, or 429
status. Unix and WebSocket use the shared UI protocol response.

Stable user linking uses the separate
[`identity-binding-protocol.md`](identity-binding-protocol.md) contract. Its
serializable requests never carry a transport principal. A concrete Channel
derives a non-serializable identity envelope only after ingress authentication,
and the Runtime re-authorizes it before accessing its private binding store.

## Audit and data minimization

Authorization denials are persisted even when optional run-content evidence is
disabled. Each record contains time, request ID, channel instance, transport,
operation, code, and SHA-256 digests for principal/resource identifiers. It
does not contain bearer tokens, raw messages, session IDs, prompts, tool input,
or tool output. Retention and export use the runtime evidence controls.

## Configuration

An Agent is private by default:

```toml
[agents.access]
allow_authenticated = false
allowed_principals = ["local-owner", "telegram:primary:123456"]
allowed_roles = ["operators"]
```

HTTP and WebSocket require both a principal and a secret reference:

```toml
[channels.transport]
kind = "websocket"
bind = "127.0.0.1:9527"
principal_id = "desktop-owner"

[channels.transport.bearer_token]
source = "env"
name = "SYLVANDER_DESKTOP_TOKEN"
```

`allow_authenticated = true` is convenient for a single-user deployment but
widens access to every authenticated channel principal. Multi-user or
multi-bot deployments should use explicit principal or role allowlists.

## Current-schema and rollback rules

- HTTP and WebSocket entries require `principal_id` and a bearer-token secret
  reference. Missing fields fail configuration validation.
- Unix identity always comes from peer credentials under the configured
  channel instance. No environment UID or anonymous-local fallback exists.
- Durable platform sessions require `channel_instance_id`, current ownership,
  and current effective-configuration state. Missing or old shapes fail closed
  instead of being claimed or rewritten.
- Rollback is code-reversible only to a build that accepts the same current
  durable schema. Removing this boundary reopens cross-session and
  cross-instance access; disable external listeners and preserve the denial
  audit before any authorized rollback.
