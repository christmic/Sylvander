# `sylvander-channel` architecture

`sylvander-channel` is the transport-neutral ingress boundary for Sylvander.
It does not run an Agent, persist configuration, or decide authorization. Its
job is to normalize an external interaction, preserve the authenticated
transport identity, and submit the operation to the Runtime-owned `UiService`.

## Ownership

```text
adapter (Unix / HTTP / WebSocket / chat bot)
  -> ExternalChatRequest + BoundaryContext
  -> ChannelContext / UiService
  -> Runtime
  -> Agent engine + durable session store
```

The adapter owns protocol framing, provider authentication, inbound size
limits, delivery formatting, and its native connection lifecycle. The Runtime
owns agent selection, stable identity resolution, session creation, policy,
approval, and durable evidence. A channel must never create an `AgentRun`,
pick a user identity from model input, or write a session store directly.

## Core contracts

- `Channel` is the lifecycle contract consumed by Runtime supervision. A
  channel receives `ChannelContext`, marks readiness only after its ingress is
  usable, and exits with an error for the supervisor to classify and restart.
- `ExternalChatRequest` is the normalized, authenticated input to
  `UiService::submit_chat`. The caller supplies text, selected Agent, optional
  session, and requested overrides; the Runtime validates their authority.
- `AuthenticatedTransportIdentity` is the transport-scoped source identity.
  It is deliberately distinct from the durable `UserId`; principal binding is
  a Runtime operation.
- `parse_external_control` recognizes only the explicitly supported chat
  controls. It does not give an adapter a generic administrative backdoor.

## Correctness boundaries

1. A `BoundaryContext` is constructed at ingress and carried unchanged through
   the submission path. The model never supplies its transport, channel
   instance, principal, or request ID.
2. `submit_external_chat` uses the Runtime's authenticated UI service. Failure
   compensation belongs to that service, so an adapter cannot leave a durable
   session without an attached Agent run.
3. Channel instances are identified by stable `instance_id`, not just a
   transport kind. This keeps replay keys, outbound subscriptions, and session
   routing isolated across multiple bots of the same provider.
4. The common crate contains no provider tokens, socket paths, or HTTP server
   state. Those remain in the adapter crate and Runtime configuration.

## Adding an adapter

1. Implement `Channel` in a dedicated `sylvander-channel-*` crate.
2. Turn native requests into `ExternalChatRequest` and establish an honest
   `BoundaryContext`; do not invent authenticated principals.
3. Subscribe only to the configured Agent and instance-owned sessions before
   rendering outbound events.
4. Return bind, protocol, or delivery failures to Runtime supervision instead
   of retrying indefinitely inside the adapter.
5. Add the adapter's module reference under `docs/` and update
   [`../../docs/INDEX.md`](../../docs/INDEX.md).

## Related documentation

- [`../../docs/boundary-authorization.md`](../../docs/boundary-authorization.md)
  — authentication and authorization ownership.
- [`../../sylvander-runtime/docs/channel-supervision.md`](../../sylvander-runtime/docs/channel-supervision.md)
  — instance lifecycle and bounded restart policy.
- [`../../docs/module-sylvander-channel-unix.md`](../../docs/module-sylvander-channel-unix.md)
  — local TUI transport reference.
