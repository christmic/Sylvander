# User Profile protocol

This document is the normative public contract for a user's global interaction
profile. Protocol DTOs live in `sylvander-protocol`; authentication,
authorization, persistence, audit, retention, and prompt composition remain
Runtime responsibilities.

## Ownership and trust boundary

A `UserProfileRequest` never contains `user_id`, `owner_user_id`, a transport
principal, or any equivalent selector. Runtime derives the stable `UserId`
from an authenticated boundary and applies the action to that owner only:

```text
authenticated boundary
  -> stable PrincipalBinding
  -> Runtime-derived UserId
  -> validate UserProfileRequest
  -> owner-scoped UserProfileStore
```

Clients cannot read, mutate, export, correct, or delete another user's profile
by guessing an identifier. Transport display names are never identity. An
unauthenticated or unresolved boundary fails closed before store access.

## Latest-only version and capability

The current and only protocol version is `1`. Unknown versions and fields are
rejected; there is no legacy decoder, fallback, dual read/write, or migration
contract. Both UI peers must advertise `user_profile_v1`, and Runtime must
advertise `UserProfileCapabilities::current()`. The default capability set is
empty and denies every operation.

Profile entities also have an optimistic `revision`. Every mutation after
creation requires a non-zero `expected_revision`. Runtime reports conflicts
through the typed public error and may expose only the current revision, never
storage details.

## Typed preferences

`UserProfileData` is a complete replacement payload with no arbitrary JSON
extension map. It currently supports:

- preferred language and locale;
- response detail (`concise`, `balanced`, `detailed`);
- communication tone (`direct`, `warm`, `formal`);
- screen-reader, reduced-motion, and high-contrast accessibility preferences;
- at most 16 bounded user-owned interaction constraints.

Every preference carries one `PrivacyClass`:

| Class | Intended handling |
|---|---|
| `personal` | Owner-specific preference usable for the owner's interaction |
| `sensitive` | Minimize use and disclosure; exclude from diagnostics and broad retrieval |
| `restricted` | Use only at an explicitly authorized owner-facing boundary |

Privacy class is policy input, not authorization by itself. Runtime and the
Guardian must enforce it. Profile data and exports redact their `Debug`
representations regardless of class.

## Operations

| Operation | Client fields | Semantics |
|---|---|---|
| `create` | complete profile | Create revision 1; fail if one exists |
| `read` | none | Read the boundary owner's current profile |
| `update` | expected revision, complete profile | Ordinary owner-authorized replacement |
| `export` | `json` | Produce a portable, self-describing owner export |
| `correct` | expected revision, complete profile | Explicit data-subject correction |
| `delete` | expected revision | Erase the profile while preserving the learning opt-out |
| `set_do_not_learn` | expected revision, enabled | Change the durable learning prohibition |

`correct` is intentionally distinct from `update`: Runtime must audit it as a
data-rights action. Export payloads contain no owner identifier because the
authenticated delivery boundary already establishes ownership.

## Do-not-learn and deletion invariant

`do_not_learn = true` prohibits creating new learned profile facts,
Relationship Memory observations, Agent private candidates derived from the
user, or cross-user canonical memory derived from the user. It does not erase
existing data; deletion and correction are separate explicit controls.

Deleting a profile must not silently revoke an existing opt-out. Runtime keeps
a minimal owner-scoped tombstone and returns `do_not_learn_preserved: true`.
Re-creating a profile must inherit that marker until the owner explicitly
changes it through `set_do_not_learn`. The tombstone contains no preference
content.

## Public errors and content safety

`UserProfileError` contains only a stable code, operation, optional current
revision, and optional retry delay. Database paths, SQL, provider errors,
profile values, transport principals, and internal policy reasons never cross
this boundary. Inputs are bounded during deserialization and validation;
unknown fields fail closed.

## Runtime integration status

The production path currently includes:

- a Runtime-owned durable SQLite store with exact latest-schema validation;
- boundary-derived stable ownership and owner-free requests;
- optimistic revision compare-and-swap, restart continuity, export,
  correction, deletion, and the durable opt-out tombstone;
- Unix-domain socket and WebSocket routing through the Runtime UI service;
- `user_profile_v1` capability negotiation and content-safe public errors;
- Evidence-backed, content-safe operation outcomes in the Runtime path.

The TUI advertises and negotiates `user_profile_v1`, but does not yet provide a
profile editor. Protocol support must not be described as an implemented TUI
editing experience.

The Agent crate contains a deterministic, bounded formatter for the compact
User Profile interaction contract. It is not yet injected by the live
per-turn Runtime/Agent prompt path. The durable `do_not_learn` marker is also
not yet enforced across every Relationship Memory and Agent private-candidate
write. Those are open integration gates, not implied by storage or wire
support.

Completion still requires tests proving:

1. the live turn snapshot injects the owner profile with revision and digest
   provenance in the documented prompt precedence order;
2. an active `do_not_learn` marker denies every governed learning write while
   leaving explicit correction, export, and deletion available;
3. audit failure and profile-store failure remain fail-closed without leaking
   profile, SQL, path, or principal content;
4. the future TUI editor sends operations only after capability negotiation
   and handles typed revision conflicts without overwriting newer data.

Operational database placement, backup, permissions, and tombstone retention
are specified in
[`server-configuration.md`](server-configuration.md#global-user-profile).

The generated schema is available through
`schema::user_profile_protocol_schema()` and is embedded under `user_profile`
in the complete UI schema.
