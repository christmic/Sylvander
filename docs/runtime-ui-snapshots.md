# Runtime UI snapshots

> Status: accepted design; implementation is tracked in dependency order.

## What this defines

Runtime owns the redacted snapshots used by interactive clients. A Channel
authenticates, authorizes, and serializes them; it must not reconstruct Runtime
state from adapter configuration. Desktop and TUI project the same public
facts and never become a recovery or discovery source of truth.

This contract covers two queries:

1. `GetRuntimeInfo { agent_id }` returns the current provider-qualified model,
   model catalog, reasoning effort, effective Runtime permission profile,
   approval availability, request byte limit, and platform snapshot for one
   visible Agent.
2. `ListSessions { include_archived }` returns visible persistent Sessions and
   their archive state. The normal product query passes `false`; an explicit
   archive browser passes `true` and may then issue `RestoreSession`.

## Why Runtime owns both snapshots

The queried values combine mutable Agent state, authorization, storage, and
boundary policy. Unix previously received a separately assembled
`RuntimeInfo`; WebSocket had no equivalent path. Session listing previously
called a boot-loader API that intentionally hides archived records. Those
shapes made transport parity impossible and encouraged clients to cache facts
that Runtime can revoke or change.

One Runtime operation per query provides:

- identical authorization and redaction across Unix, WebSocket, and future
  service protocols;
- provider-qualified model identity without guessing from a model name;
- live platform health rather than a startup copy;
- an explicit, authorized archive discovery path;
- one request-size fact matching the boundary that actually enforces it.

## Public DTOs

`RuntimeUiSnapshot` is a data-only API value. It contains no model client,
credential, tool executor, sandbox handle, store, or callback.

`UiSessionInfo.archived` is required. A Session returned from the normal query
has `archived=false`; archive browsing may return both states. The query flag
controls visibility, not authorization: Runtime still resolves the stable
user, checks every Agent binding, and rejects invisible Sessions.

The old `max_attachment_bytes` response field is removed. Runtime currently
enforces a serialized request limit, not an independent decoded attachment
limit, so the public snapshot reports `max_request_bytes`. A future attachment
quota must be introduced together with matching Runtime validation.

## Execution flow

```text
Desktop/TUI command
  -> Channel protocol negotiation
  -> ChannelHost operation
  -> Runtime boundary authorization
  -> active Agent or SessionStore query
  -> redacted API snapshot
  -> Channel serialization
  -> client projection
```

For restore:

```text
ListSessions(include_archived=true)
  -> select archived Runtime row
  -> RestoreSession(session_id)
  -> SessionUpdated(archived=false)
  -> ListSessions(include_archived=false)
```

Desktop does not keep an archived Session alive after Runtime removes it from
the active list. Restore begins only from a fresh archive-inclusive response.

## Invariants

- Channels do not hold a second model catalog, permission profile, platform
  callback, or request limit for UI reporting.
- Model identity is always `provider_id + model_id`.
- Archived discovery is opt-in and remains ownership-filtered.
- A list response completely replaces the matching client projection; cached
  rows cannot authorize restore or deletion.
- Runtime returns only public data and performs no UI rendering.
- New Rust imports remain module-scoped.

## Verification gates

1. API serialization and schema tests prove the new exhaustive command and
   DTO shapes, including provider-qualified duplicate model names.
2. Runtime tests prove active-only and archive-inclusive ownership filtering,
   plus live model/platform/request-limit projection.
3. Unix and WebSocket tests prove identical snapshots and command routing.
4. Desktop tests prove archive discovery and fact-driven restore without an
   optimistic local mutation.
5. Strict all-target Clippy, warning-denied Rustdoc, architecture verification,
   and the indented-`use` scan pass for every modified Rust file.

