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

## Storage

If `server.data_dir` is omitted, it resolves to
`$XDG_DATA_HOME/sylvander`, `~/.local/share/sylvander`, or
`.local/share/sylvander`, in that order. The default session database and
workspace journal live below that directory. Explicit paths remain useful for
containers, backups, and migration drills.

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
Retention policy revision starts at 1 and is persisted with every row. Any
policy change must increase it; changing policy values under the same revision
fails startup instead of silently reinterpreting existing memory.

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
routing are scoped to the configured instance. Full route policy, interactive
channel decisions, retry/backoff, and per-instance operational health remain
tracked in P4. See
[`boundary-authorization.md`](boundary-authorization.md) for authentication,
Agent access policy, limits, audit, and migration requirements.

## Capability names

Supported model capabilities are:

- `tool_use`
- `vision`
- `document_input`
- `extended_thinking` or `reasoning`
- `prompt_caching`
- `structured_output`

Unknown values fail Agent composition rather than being silently ignored.
