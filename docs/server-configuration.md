# Server configuration

Sylvander's production server is configured by one versioned TOML document.
Set `SYLVANDER_CONFIG` to its path:

```sh
export SYLVANDER_CONFIG=/etc/sylvander/server.toml
sylvander
```

When the variable is absent, the server converts the legacy environment
contract into the same in-memory schema. This retained path is an explicitly
approved, bounded migration exception; it is not precedent for new fallback or
automatic migration behavior. New deployments should use TOML.

The maintained example is
[`config/sylvander.example.toml`](../config/sylvander.example.toml).

## Startup contract

Startup is fail-fast and ordered:

1. parse a document no larger than 1 MiB;
2. reject unknown fields, schema versions, duplicate identities, dangling
   references, unsupported providers, and invalid limits;
3. resolve secret references without serializing secret values;
4. open the durable session database;
5. construct and subscribe every configured Agent;
6. restore persistent sessions with their original IDs;
7. construct enabled channel instances and begin accepting traffic.

No channel accepts traffic when an Agent, model, secret, bind address, or
session store fails to initialize.

## Execution targets

Built-in coding tools resolve the session's exact execution target on every
turn. `local` executes below the configured local root. An SSH target uses the
system OpenSSH client with batch mode, a bounded operation timeout, and one
configured identity. Its `credential` secret resolves to the absolute path of
that identity file; it is a path reference, not private-key content. For
example:

```toml
[[execution_targets]]
id = "build-host"

[execution_targets.transport]
kind = "ssh"
host = "build.example.com"
port = 22
user = "builder"
known_hosts = "/etc/sylvander/ssh_known_hosts"
control_path = "/var/run/sylvander/ssh-%C"
worktree_root = "/srv/sylvander/worktrees"

[execution_targets.transport.credential]
source = "env"
name = "SYLVANDER_SSH_IDENTITY_PATH"
```

`known_hosts` is a deployment-owned file and strict verification is mandatory;
Sylvander never learns a host key interactively. `control_path` enables bounded
OpenSSH connection sharing (`ControlMaster=auto`, `ControlPersist=60`) and its
parent must already be private to the server account. `worktree_root` is an
absolute, non-root directory on the remote host used for durable coding-session
branches. Workspace paths for SSH targets are also absolute remote paths.
Unknown targets fail explicitly before a tool can fall back to the server
filesystem. A read-only workspace permits reads and rejects writes, edits, and
commands.

A container target runs every workspace operation in a fresh disposable
container. The server bind-mounts the selected host workspace at `/workspace`,
sets that directory as the working directory, keeps stdin attached, removes
the container after the operation, and disables container networking. The
runtime is one executable (for example `docker` or `podman`), not a shell
command with embedded flags:

```toml
[[execution_targets]]
id = "rust-toolchain"

[execution_targets.transport]
kind = "container"
runtime = "docker"
image = "rust:1.90-bookworm"

[execution_targets.transport.resources]
memory_mb = 2048
cpu_millis = 2000
pids_limit = 512
```

Reads, list/search, and trusted inspection commands use a read-only bind mount.
Writes and ordinary commands require a writable workspace binding. Command
stdout/stderr use the same bounded head/tail capture and live progress contract
as local and SSH execution. Each operation has a deadline and terminating the
Agent turn drops and kills its runtime process. Because the bind source is a
server-host workspace, clean writable Git workspaces receive the same default
session worktree isolation as the local execution target before being mounted
into the container.

Every operation is also started with a read-only root filesystem, a private
64 MiB `/tmp`, no added capabilities, `no-new-privileges`, and explicit
memory, CPU, and process ceilings. Resource values are validated at startup;
the defaults shown above apply when `resources` is omitted.

A managed sandbox uses the same disposable, restricted OCI execution contract
but gives the executable and image policy-oriented names:

```toml
[[execution_targets]]
id = "review-sandbox"

[execution_targets.transport]
kind = "sandbox"
driver = "podman"
profile = "sylvander/review-sandbox:latest"

[execution_targets.transport.resources]
memory_mb = 4096
cpu_millis = 4000
pids_limit = 768
```

`driver` is one OCI-compatible executable and `profile` is one immutable image
reference. Sylvander deliberately does not reuse containers between
operations: disposable environments prevent session state and credentials
from leaking into a later Agent operation. Writable Git coding sessions retain
their durable state in an isolated host worktree, not in a long-lived
container.

## Agents, providers, and models

`model_providers` contains credentials and a catalog of model capabilities.
An Agent's `spec.model.provider` and `spec.model.model_name` select its default.
The runtime constructs a separate provider client for each Agent and exposes
that provider's model catalog to compatible clients.

`agents[].revision` identifies the immutable definition revision.
`default_prompt_profile` selects an additional provider/model layer; it does
not replace the Agent persona. Durable sessions store sparse overrides
separately from their fully resolved effective configuration. Every turn
atomically snapshots the Agent revision, provider/model, reasoning,
permissions, prompt manifest/profile, workspaces, execution target, and
per-field provenance before provider or tool work begins. Runtime updates
require the caller's expected configuration revision so concurrent clients
cannot silently overwrite each other.

Agents may compose additional dependency and artifact workspaces beside their
home and the session task workspace:

```toml
[[agents.workspace_mounts]]
reference = "shared-lib"
role = "dependency"

[agents.workspace_mounts.binding]
execution_target = "local"
path = "/srv/dependencies/shared-lib"
read_only = true
instruction_focus = "packages/api"

[agents.workspace_mounts.capabilities]
read = true
git = true
write = false
command = false
```

Unqualified file paths use the task workspace. Other mounts use
`@reference/path`; Command and Git accept `workspace = "reference"`. Logical
references must be unique. Explicit dependency/artifact target-path overlap is
rejected; Agent home and task may intentionally alias the same location. The
effective session configuration exposes every mount and capability policy for
UI inspection. `instruction_focus` is relative to the binding root; Sylvander
loads one canonical instruction alias per ancestor from the root to that focus.

## Prompt resolution and privacy

The runtime has one deterministic resolver for session creation, restart, and
execution. It composes non-empty layers in this order, separated by two
newlines:

1. the built-in, non-overridable Sylvander safety floor;
2. the selected provider/model prompt profile, when configured;
3. the Agent persona prompt;
4. the session prompt input, only when `allow_session_prompt = true`.

A profile should use `qualified_models`, for example:

```toml
[[agents.prompt_profiles]]
id = "coding"
qualified_models = [{ provider_id = "primary", model_id = "claude-sonnet" }]
system_prompt = "Prefer small, verified coding changes."
```

Qualified selectors match the exact `(provider_id, model_id)` pair. The legacy
`providers` and `models` fields are accepted only when both are empty
(universal profile) or when they contain exactly one provider and one model.
They never form a cross product and never fall back across same-named models.

There may be at most 32 profiles per Agent and 64 selectors per selector kind.
Agent and profile prompt layers are limited to 64 KiB each, session input to
16 KiB, and the final resolved prompt to 128 KiB. Forbidden control characters
and non-canonical identifiers fail validation with content-free errors.

The effective configuration records layer kind, safe reference, SHA-256,
byte count, a framed aggregate digest, and the final prompt SHA-256. Before any
turn record, history mutation, compaction request, tool, or model request, the
Agent resolves the prompt again and requires the digest and manifest to match
the durable snapshot exactly. Missing legacy manifests and modified digests
fail closed.

Raw Agent/profile prompts are never returned by administration reads. Session
prompt input is write-only through the public UI protocol: configuration
responses omit it while retaining the manifest digest and byte count for
inspection. Debug formatting also redacts it. Operators must still treat the
session database as sensitive because it contains the durable input needed to
reproduce authorized sessions.

## Agent revision administration

Agent definitions are administered through the public UI service protocol.
`UpdateDefinition` validates and fully composes a candidate before it creates
an immutable, inactive revision. `ActivateRevision` and `RollbackRevision`
move the active head separately, so storing a candidate never changes live
behavior. Every mutation includes `expected_active_revision`; a stale caller
receives a typed conflict and the active head remains unchanged.

Inspection is deliberately redacted. It exposes digests and safe metadata, not
raw prompts, workspace paths, command arguments, or secret references. Stored
identity or digest corruption fails closed. Administration requires an admin
or system principal, and every attempted mutation produces content-free audit
evidence with a success or failure outcome.

New sessions bind the active revision's execution composition. Existing
sessions keep their historical model, prompt, tools, and runtime worker across
activation and restart. The current active safety/access policy remains a live
server floor and may revoke access to an older session immediately.

## Stable user identity binding

Stable identity binding is optional and fail-closed. It is enabled only when
`server.identity.digest_key` is configured as an environment/file secret
reference. Runtime then owns a latest-schema SQLite store at
`server.identity.database` (default: `<data_dir>/identity.db`) and advertises
`identity_binding_v1`. Without the key, it advertises no identity capability.

```toml
[server.identity]
challenge_ttl_seconds = 300
# database = "/var/lib/sylvander/identity.db"

[server.identity.digest_key]
source = "env"
name = "SYLVANDER_IDENTITY_DIGEST_KEY"

[[server.identity.trusted_issuers]]
transport = "unix"
channel_instance_id = "terminal"
principal_id = "local-alice"
user_id = "alice"
```

Each trusted issuer is one exact authenticated ingress permitted to issue a
link code for its configured stable user. Multiple issuers may map to the same
user, but duplicate ingress triples are rejected. Link requests cannot carry a
user, transport, channel, or external principal. TTL is bounded to 30–900
seconds. The digest key must contain 32–4096 bytes; its value, raw external
principal IDs, and one-time secrets are never persisted or emitted by Debug.

The user requests a code through a trusted issuer and confirms it through the
external channel being linked. Resolve and CAS unlink always apply to the
authenticated ingress-derived external identity. See
[`identity-binding-protocol.md`](identity-binding-protocol.md).

## Global User Profile

Runtime always opens one owner-scoped User Profile SQLite database. Configure
it with `server.user_profile_db`; when omitted it resolves to
`<data_dir>/user-profiles.db`:

```toml
[server]
data_dir = "/var/lib/sylvander"
user_profile_db = "/var/lib/sylvander/user-profiles.db"
```

The database accepts only the exact latest schema (`application_id` plus
`user_version = 1` and an exact schema-object comparison). An empty database
is initialized once. An old, unknown, modified, or corrupt schema fails
startup; Runtime does not migrate, repair, downgrade, or fall back to an
in-memory store.

`user_profile_v1` is the public UI capability. Unix-domain socket and
WebSocket channels route the strict `UiClientMessage::UserProfile` envelope to
Runtime, which derives its owner from the authenticated boundary. Both peers
must advertise the capability before a client uses it. The TUI currently
advertises and negotiates the capability but does **not** yet expose a profile
editing surface; use a protocol client until that UI is implemented.

Operational requirements:

- create the database directory with access limited to the Sylvander service
  account (normally directory mode `0700` and database mode `0600`), and do not
  grant channel or Agent processes direct SQLite access;
- include `user-profiles.db` in encrypted, access-controlled backups; the
  current store has no dedicated online-backup lifecycle, so stop/quiesce the
  Runtime and copy the database as one unit rather than copying it during a
  write;
- test restore into an isolated data directory and require Sylvander startup
  to pass exact-schema and SQLite integrity validation before promotion;
- treat owner exports and the database as personal data even though Debug and
  public errors redact profile values.

Profile deletion removes preference content but deliberately retains a
minimal owner-scoped tombstone with `do_not_learn = true`. Backups and restore
procedures must retain that tombstone; dropping it can silently re-enable
learning after profile recreation. The owner may change the marker only
through the explicit versioned protocol operation.

Runtime currently requires the Evidence store for profile operations and
records content-safe administration evidence for their outcome. The
deterministic, bounded User Profile prompt formatter exists in the Agent
crate, but the live per-turn Agent prompt path does not yet inject that layer.
Likewise, `do_not_learn` is durable protocol/storage state, not yet a complete
enforcement gate across Relationship Memory and Agent candidate creation. Do
not claim those latter controls are active until their Runtime call paths and
acceptance tests land. See
[`user-profile-protocol.md`](user-profile-protocol.md) for the wire contract and
the live completion boundary.

## Secret references

Credentials cannot be embedded as TOML literals. A secret is either:

```toml
source = "env"
name = "PROVIDER_API_KEY"
```

or:

```toml
source = "file"
path = "/run/secrets/provider-api-key"
```

Secret files must be regular files no larger than 64 KiB. Resolved values are
redacted from formatting and cleared from their temporary owned buffer after
client construction. Do not put credentials in command-line arguments,
committed examples, logs, or Agent prompts.

## Relationship-memory integrity anchor

Production relationship memory requires one common integrity `key` secret
reference and exactly one typed backend. Existing flat `anchor_path`
configuration is not accepted; Sylvander is pre-release and reads only the
latest configuration shape.

The file backend protects against a database writer that cannot modify the
external anchor:

```toml
[server.memory_maintenance.integrity]

[server.memory_maintenance.integrity.key]
source = "env"
name = "SYLVANDER_MEMORY_INTEGRITY_KEY"

[server.memory_maintenance.integrity.backend]
kind = "file"
anchor_path = "/var/lib/sylvander-integrity/anchor.json"
```

The path must be absolute, outside `server.data_dir`, and beneath an existing
directory with a separately administered write boundary. It does not resist a
host administrator replaying the database, anchor, and key together.

Use the remote monotonic CAS backend for that stronger threat model:

```toml
[server.memory_maintenance.integrity]

[server.memory_maintenance.integrity.key]
source = "env"
name = "SYLVANDER_MEMORY_INTEGRITY_KEY"

[server.memory_maintenance.integrity.backend]
kind = "http"
endpoint = "https://memory-anchor.example.test/v1/cas"
timeout_millis = 5000
read_retries = 3

[server.memory_maintenance.integrity.backend.bearer_token]
source = "env"
name = "SYLVANDER_MEMORY_ANCHOR_TOKEN"
```

Only HTTPS endpoints are accepted. Credentials, query parameters, and
fragments are forbidden in the URL. `timeout_millis` is bounded to 100–30000;
`read_retries` is bounded to 0–3. Private-PKI deployments may add
`backend.ca_certificate` and `backend.client_identity`, each as a `SecretRef`.
The endpoint credentials and TLS references are not rendered in Debug output
or validation errors. Read retries are bounded; compare-and-swap conflicts and
ambiguous mutations fail closed instead of being blindly replayed.

The remote service contract is deliberately small and strongly consistent:

- `GET` returns `200`, the signed anchor JSON body, and a strong `ETag`; `404`
  means the resource has never been created.
- Bootstrap uses `PUT` with `If-None-Match: *`. An existing resource must return
  `409` or `412`, never overwrite the current value.
- Every transition uses `PUT` with the exact strong `ETag` in `If-Match` and
  returns a new strong `ETag`. Stale revisions return `409` or `412`.
- Successful writes return `200` or `201`. Writes are not automatically
  retried after timeout because the commit result is ambiguous; the next
  startup/read resolves a durable `Pending` state against the database root.

The service must durably linearize the value and revision in one transaction.
Do not place a cache, eventually-consistent object store, or CDN in this path.
The optional client identity secret is one PEM document containing the client
certificate chain and private key. Operate the service, its storage, and its
credentials outside the database host's administrative rollback boundary.

This defeats replay of the database, local file anchor, integrity key, and
local configuration to an older valid snapshot. It does not defend against an
administrator actively controlling both the live Sylvander process and the
remote anchor service or its current write credentials; that is an operational
separation and credential-lifecycle boundary.

## Storage

If `server.data_dir` is omitted, it resolves to
`$XDG_DATA_HOME/sylvander`, `~/.local/share/sylvander`, or
`.local/share/sylvander`, in that order. The default session,
relationship-memory, and User Profile databases and the workspace journal live
below that directory. Explicit paths remain useful for containers, backups,
and restore drills.

`server.evidence` controls the structured run ledger. It is enabled by default
with a 30-day retention declaration and `metadata_only` content policy. The
other policies are `redacted` and `full`; `full` is an explicit operator choice
and must be paired with appropriate access, deletion, and backup controls.
The ledger is evidence for review and evaluation, never permission for the
Agent to change or deploy itself without the gated workflow in P5.
See [`runtime-evidence.md`](runtime-evidence.md) for the data model, recovery,
retention, query, and self-improvement boundary.

`server.memory_maintenance` declares the bounded production retention policy
for durable Agent memory. The declared defaults are a 365-day TTL, a maximum
TTL of 1825 days, a 7-day expired-row recovery grace, and 30-day retention for
superseded rows. The maintenance budget is hourly batches of 500, with at most
20 batches per run, and no more than 1000 rows in one batch. Every value is
finite and range-checked; unknown fields and configurations where
`default_ttl_days` exceeds `max_ttl_days` fail startup.
There is no unbounded or legacy-environment fallback. Runtime executes
retention and scheduled backup rotation in one maintenance lifecycle. Backup
cadence is finite: one day by default with seven retained copies, bounded to
1–7 days and 2–30 copies. A new backup is published and exactly verified before
older copies are removed. Only schema-, integrity-, and manifest-verified
database/manifest pairs count toward rotation; temporary, orphaned, corrupt,
or unknown artifacts are ignored. Failures use content-safe diagnostics and
retry on the next scheduled interval without replacing the last valid copy.
The backup directory is derived beneath `data_dir`; configuration cannot route
memory snapshots to an arbitrary filesystem path. Restore remains an explicit
offline operator action: Runtime never restores or falls back automatically.

Each scheduled backup run also bounds the relationship-memory audit and
retention ledgers. Runtime first publishes and verifies a signed backup whose
epoch and database root exactly equal the committed external anchor. Only that
artifact can authorize one maintenance-only compaction batch. Missing,
modified, forged, or older-epoch artifacts fail closed. After every non-empty
batch, Runtime publishes another verified backup before continuing, so the
compacted live database always has a current, offline-restorable artifact even
when the batch budget is exhausted or shutdown follows immediately.

One batch deletes at most `batch_size` audit rows and `batch_size` paired
retention run/batch records. The newest row in each ledger is retained as a
live continuity boundary. Deleted rows are folded, in deterministic order,
into domain-separated cumulative summary roots. Counts, roots, checkpoint
epoch/root, and backup digest live in one constant-size checkpoint row covered
by the same external anchor. Backup rotation may eventually remove the older
artifact containing individual compacted rows; the cumulative root preserves
cryptographic evidence of those rows, not their plaintext inspection history.
Deployments that require indefinite row-level inspection must export signed
backups to a separately governed archive before rotation.

Ordinary SQLite writes cannot delete audit or retention rows or write the
checkpoint accumulator: exact-schema triggers require an in-process,
thread-scoped maintenance gate. Transaction failure rolls back deletions and
the accumulator together, while the existing pending/committed anchor protocol
covers crash recovery. A no-op convergence check does not advance the anchor,
so its just-published backup remains current.

Retention policy revision starts at 1 and is persisted with every row. Any
policy change must increase it; changing policy values under the same revision
fails startup instead of silently reinterpreting existing memory.

Production policy rollout is staged rather than activated while the memory
database opens. The stage is authenticated by the external memory anchor, but
the previous active policy remains authoritative while Runtime validates Agent
composition, starts every Agent revision, restores authenticated sessions,
opens evidence storage, and completes bounded maintenance catch-up. Runtime
activates the exact staged policy with a SQLite CAS only after all of those
fallible readiness checks succeed; no fallible startup step follows activation.
A brand-new database has no active policy and rejects memory operations until
that activation point.

An interrupted or failed rollout may leave a staged proposal, but never
reserves its revision. A later startup using the active policy removes the
stale proposal, and a new higher-revision rollout may atomically replace it.
Concurrent identical rollouts activate idempotently. Concurrent different
rollouts are serialized by the stage identifier and active base revision, so a
losing process fails closed instead of activating another process's proposal.
The stage protects rollout ordering; it does not add protection beyond the
external anchor's documented host-administrator threat boundary.

Persistent sessions retain their IDs across restart. This identity is shared
by protocol clients, channel mappings, conversation history, approvals, and
the future run ledger; replacing it during restore is a correctness defect.
Legacy sessions are migrated at boot: their prior `metadata.workspace` becomes
an explicit local user-workspace source, while current Agent defaults are
resolved and persisted without copying secrets or raw prompts into audit
fields.

## Channel instances

Every `channels` entry has a stable instance ID and one default Agent. Multiple
DingTalk or Telegram bots are represented by multiple entries with distinct
IDs and credential references. Telegram webhooks require
`X-Telegram-Bot-Api-Secret-Token` to match `webhook_secret`.

The current server can construct Unix, HTTP, WebSocket, DingTalk, Telegram,
and WeChat adapters. External principals, session mappings, and outbound
routing are scoped to the configured instance. Each entry also accepts a
`channels.supervision` table with `max_restart_attempts`,
`initial_backoff_ms`, and `max_backoff_ms`. Runtime health, readiness,
bounded restart/backoff, failure isolation, and cooperative drain are
instance-scoped. Interactive channel decisions and transport-specific
delivery retry remain tracked in P4. See
[`boundary-authorization.md`](boundary-authorization.md) for authentication,
Agent access policy, limits, audit, and migration requirements, and
[`channel-supervision.md`](../sylvander-runtime/docs/channel-supervision.md)
for the lifecycle contract.

## Capability names

Supported model capabilities are:

- `tool_use`
- `vision`
- `document_input`
- `extended_thinking` or `reasoning`
- `prompt_caching`
- `structured_output`

Unknown values fail Agent composition rather than being silently ignored.
