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

The TUI advertises and negotiates `user_profile_v1` and exposes `/profile`.
Its compact typed editor covers language, locale, response detail, tone,
accessibility, and bounded constraints without asking users to author JSON.
Read/show, create, update, explicit correction, do-not-learn on/off, JSON
export, and confirmed deletion all use this public envelope. Mutating commands
first load the server revision; a typed conflict invalidates the stale cache
and reloads rather than retrying or overwriting newer data.

The live Runtime composition injects the owner-scoped
`UserProfileProvider` into every Agent run. Before each authenticated turn, the
Agent loads the current profile, renders the deterministic bounded interaction
contract, and admits it as the typed `UserProfile` layer. Its revision and
content digest are recorded in the turn-context manifest between the Agent and
Relationship Memory layers. Provider or formatting failure aborts prompt
construction rather than silently omitting a configured profile.

The durable `do_not_learn` marker is both prompt-visible state and a
Runtime-owned authorization input. An active marker denies direct
Relationship Memory append, Worker memory-candidate invocation, Guardian event
admission, pre-existing queued-event candidate extraction, and new governed
commit actions for relationship, profile, Agent-canonical, and workspace
knowledge scopes. The profile store is queried at each of those boundaries;
an unavailable preference source fails closed. The marker does not block
explicit owner correction, export, deletion, decay, or forgetting, because
those operations govern existing data rather than learn a new fact.

Acceptance coverage proves the live turn layer order, owner-scoped profile
lookup, revision/digest provenance, restart-safe storage, opt-out before and
after event persistence, fail-closed preference lookup, memory-candidate
denial, and continued explicit correction/export/delete. The TUI capability
gate, typed request mapping, revision-bound editor, and conflict path have
unit coverage as the public client journey.

Operational database placement, backup, permissions, and tombstone retention
are specified in
[`server-configuration.md`](server-configuration.md#global-user-profile).

The generated schema is available through
`schema::user_profile_protocol_schema()` and is embedded under `user_profile`
in the complete UI schema.
