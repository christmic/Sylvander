# Identity binding protocol

This document defines the public and Channel-facing contract for linking an
authenticated external principal to a stable Sylvander `UserId`. Persistence
and Runtime policy are intentionally outside this module.

## Trust boundary

An identity request contains an action, never an external principal:

```text
authenticated transport ingress
  -> BoundaryContext established by that transport
  -> ChannelContext derives AuthenticatedTransportIdentity
  -> Runtime UiService re-authorizes boundary + typed identity
  -> Runtime-owned PrincipalBindingStore
```

`AuthenticatedTransportIdentity` has private fields, no public constructor,
and no Serde implementation. `ChannelContext::submit_identity_binding` is the
only derivation path. Its external principal is redacted from `Debug`.
Transport adapters must construct `BoundaryContext` from their actual
signature, peer-credential, token, or platform-identity authentication result;
they must never decode a client-supplied `BoundaryContext` as authority.

The Runtime must independently authorize the supplied boundary before it
consumes the identity tuple. A Channel receives neither a binding store nor a
digest key.

## Version and capability

The latest-only subprotocol version is `1`; unsupported versions fail closed.
The public UI capability is `identity_binding_v1`. Both client hello and server
welcome must explicitly advertise it. Separately, `UiService` advertises an
`IdentityBindingCapabilities` version set. Its default is empty, and the
default operation returns `service_unavailable` without reflecting request or
principal data.

No compatibility or implicit fallback path exists.

## Operations

Confirmation, resolution, and unlink apply to the ingress-derived external
principal. Begin is different: only an already authenticated stable user may
request a code, and Runtime derives that user from its trusted boundary. The
user then carries the code to the external Channel that should become linked.

| Operation | Caller-controlled fields | Success response |
|---|---|---|
| `begin` | none; target user is Runtime-derived | `challenge_issued` |
| `confirm` | bounded challenge ID and secret proof | `resolved` |
| `resolve` | none | `resolved` or `not_linked` |
| `unlink` | expected binding revision | `unlinked` |

Requests deny unknown fields and validate exact version, whitespace, control
characters, and size limits. They contain no `transport`,
`channel_instance_id`, external-principal, or target-user field. This two-sided
proof prevents an external account from selecting and taking over an arbitrary
known `UserId`.

## Secret contract

Only `challenge_issued` can contain `OneTimeIdentityLinkSecret`. That value:

- is not cloneable or displayable;
- renders as `[REDACTED]` in `Debug`;
- permits exactly one successful Serde serialization;
- is consumed into `IdentityLinkSecretProof` by the client;
- remains redacted when the confirmation request is debugged.

Binding views, ordinary acknowledgements, and public errors have no secret
slot. Runtime adapters must map storage failures to stable public error codes;
they must not return raw database, provider, digest, credential, or principal
details.

## Rollback

The commits are code-reversible. If the Runtime service is not installed, the
empty capability set and default `UiService` response preserve denial. Removing
only a transport integration cannot expose the identity store because the
Channel contract never grants store access.
