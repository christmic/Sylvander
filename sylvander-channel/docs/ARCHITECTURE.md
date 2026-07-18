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
- `CredentialLeaseSource` is the object-safe, transport-neutral boundary for
  renewable channel credentials. An adapter requests named slots for its
  stable `instance_id` at the authentication or outbound-operation boundary.
  A returned `CredentialLeaseBundle` is atomic, generation-stamped, expires
  within five minutes, clears owned bytes on drop, and never formats secret
  content.
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
5. Credential leases are exact-slot and instance-scoped. An expired,
   malformed, partially resolved, or unavailable lease rejects the current
   operation; adapters must not retain or fall back to a previous credential.

## Adding an adapter

1. Implement `Channel` in a dedicated `sylvander-channel-*` crate.
2. Turn native requests into `ExternalChatRequest` and establish an honest
   `BoundaryContext`; do not invent authenticated principals.
3. Subscribe only to the configured Agent and instance-owned sessions before
   rendering outbound events.
4. Declare each credential as a named lease slot and acquire it at the native
   operation boundary. Multi-value protocol credentials must be requested as
   one bundle.
5. Return bind, protocol, or delivery failures to Runtime supervision instead
   of retrying indefinitely inside the adapter.
6. Add the adapter's module reference under `docs/` and update
   [`../../docs/INDEX.md`](../../docs/INDEX.md).

## Verification

The common contract's white-box tests live in `tests/unit/lib.rs`. Every
adapter keeps its own protocol, authentication, replay, size-limit, identity,
and delivery tests below that adapter's `tests/` tree. A new adapter is not
complete until it proves both successful authenticated submission and
content-safe denial through the Runtime-owned UI service.

```bash
cargo test -p sylvander-channel --all-targets --locked
cargo test -p sylvander-channel-unix --all-targets --locked
cargo test -p sylvander-channel-http --all-targets --locked
cargo test -p sylvander-channel-ws --all-targets --locked
```

## Related documentation

- [`../../docs/boundary-authorization.md`](../../docs/boundary-authorization.md)
  — authentication and authorization ownership.
- [`../../docs/credential-leases.md`](../../docs/credential-leases.md)
  — renewable Provider and channel credential invariants.
- [`../../sylvander-runtime/docs/channel-supervision.md`](../../sylvander-runtime/docs/channel-supervision.md)
  — instance lifecycle and bounded restart policy.
- [`../../docs/module-sylvander-channel-unix.md`](../../docs/module-sylvander-channel-unix.md)
  — local TUI transport reference.
