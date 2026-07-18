# Credential operation audit

`credential_audit` is the Runtime-owned record of Provider and Channel
credential lifecycle operations. It answers which configured identity created,
rotated, renewed, revoked, or failed to obtain a credential revision without
storing the credential, its reference, or an arbitrary error message.

## Ownership and integration

- Production Runtime opens `<data_dir>/credential-operations.db`. This is a
  dedicated database and never shares `sessions.db`, registry tables, or
  evidence tables.
- `RegistryCredentialSource` writes Provider acquire, renew, rotation, and
  failure events on the request path. The Provider ID is stored; the credential
  binding is stored only as a SHA-256 digest.
- `CredentialRegistryMutationService` writes create, rotation, revocation, and
  failure events from the authenticated registry mutation path.
- The server receives the Runtime-owned ledger handle and injects it into each
  `SystemChannelCredentialSource`. Channel events are scoped to the stable
  configured instance ID, so multiple bots of the same transport stay
  isolated.
- A successful secret lease is not returned when its audit write fails. A
  failed credential operation keeps its original content-safe error even if
  the best-effort failure audit cannot be written.

There is no parallel audit-only credential registry or lease type. The ledger
is called by the same objects that resolve and publish the real credential
lease.

## Stored contract

The current schema is version 1 and is exact-match validated at every
operation. Old, future, partial, or foreign schemas fail closed; pre-release
latest-only policy means there is no migration or compatibility fallback.

Each row contains only:

- a random event ID;
- subject class and stable Channel/Provider identity;
- an optional SHA-256 credential-binding identifier;
- one fixed operation class;
- an optional positive credential revision;
- Unix event time;
- one fixed result code and its fixed content-safe summary.

The public record API does not accept secret bytes, secret references, raw
errors, or caller-authored summary text. `Debug` output for the ledger is
content-free.

## Query, retention, and deletion

Queries require one validated subject and return at most 200 newest events.
They never return rows for another Channel instance or Provider. Events are
retained for 90 days. Every append deletes at most 500 expired rows in the same
transaction; repeated live operations therefore drain an old backlog without
an unbounded lock. This finite policy cannot be disabled through runtime
configuration. Removing a Provider or Channel does not immediately erase its
audit history; expiry is the sole deletion rule.

## Verification

The tests in `tests/unit/credential_audit.rs`, `tests/unit/credential_lease.rs`,
`tests/unit/registry_admin.rs`, and `sylvander-server/tests/unit/credential.rs`
cover exact schema rejection, restart continuity, identity isolation, digest-
only binding persistence, bounded retention deletion, and the real Provider,
registry mutation, and Channel execution paths.
