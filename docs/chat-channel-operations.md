# Chat channel operations

DingTalk, Telegram, and WeChat Work are production chat adapters over the
common Runtime UI service. Each configured bot is a separate channel instance
with isolated renewable credentials, external identity, session mapping,
replay cache, routing defaults, health, and restart policy.

## Interactive controls

When an Agent needs a decision, the adapter includes a request ID and a command
that can be sent as an ordinary chat message:

```text
/approve <request-id> [once|session|persistent]
/deny <request-id> [reason]
/answer <request-id> <answer>
/interrupt
```

Unknown slash commands remain normal chat input. Recognized controls require
an existing instance-owned session. The adapter converts them to the public UI
control message; Runtime still verifies authenticated principal, session
ownership, operation policy, approval scope, and active request identity.
Transports do not publish approval messages directly to the Agent bus.

## Delivery

- Telegram verifies the configured webhook secret, rejects duplicate update
  IDs with a bounded expiring cache, chunks output on Unicode character
  boundaries, and bounds tool summaries without splitting UTF-8.
- DingTalk rejects duplicate message IDs with a bounded expiring cache and
  uses the instance's session webhook for outbound delivery.
- WeChat verifies and decrypts callbacks against the configured enterprise,
  rejects duplicate message IDs, acquires an API access token for the current
  credential generation, and sends bounded completed replies plus tool/control
  status through the active message API. Expired-token responses trigger one
  refresh; streamed text deltas are not emitted as duplicate messages.
- All three adapters retry network failures, HTTP 429, and HTTP 5xx at most three
  times with short increasing delays. Other HTTP 4xx responses fail
  immediately. Exhaustion is logged without crashing the Runtime supervisor.
- Inbound request/frame limits come from the server boundary configuration.

## Other adapters

Unix and WebSocket expose the complete typed UI protocol directly. HTTP uses
bounded authenticated chat ingress. All adapters share stable instance
registration, Runtime readiness, health, restart/backoff, failure isolation,
and cooperative drain. The TUI remains a single-session Unix client; Ghostty
owns multi-session presentation.
