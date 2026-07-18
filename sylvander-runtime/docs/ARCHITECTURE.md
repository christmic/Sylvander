# `sylvander-runtime` architecture

`sylvander-runtime` is the composition and ownership layer for the Sylvander
server. It turns versioned configuration into durable stores, Agent revisions,
provider routing, channel instances, and auditable control operations. It is
the only layer that may establish trusted execution identity from an external
transport.

## Composition graph

```text
ServerConfig + SecretResolver + optional external Provider lease source
  -> Runtime::boot_config
  -> durable stores (sessions, memory, evidence, identity, Guardian curation)
  -> Agent registry + provider registry
  -> typed turn-context providers + immutable actor capability snapshots
  -> authenticated UiService
  -> channel supervisor
  -> AgentRunEngine
```

The server binary supplies configuration and process lifetime only. Individual
channels own their native protocol adapters; the Agent crate owns one run;
Runtime owns the binding between them.

## Module responsibilities

- `config` validates latest-version configuration, resolves declarative
  references, and rejects unsupported legacy shapes rather than guessing.
- `composition` builds configured Agent revisions, default tools, prompt
  layers, and selected provider adapters from Runtime-owned inputs.
- `agent_registry` and the private registry modules make Agent/model revisions
  immutable for a run and expose administrator-facing updates through explicit
  revision checks. A new database atomically creates only the current catalog
  and V3 snapshot schema with one current ledger row. Old, mixed, future, or
  damaged schemas are rejected during open-time fingerprint validation without
  migration.
  Runtime deliberately shares `sessions.db` with the session store but not
  schema ownership: it opens the exact session schema first, then opens the
  registry with the session store's complete current object-name allowlist.
  Each component exact-validates its own SQL and foreign keys; only the exact
  two-owner namespace union is accepted. Standalone opens accept only the
  owner's object set, and profile, memory, evidence, Guardian, unknown,
  partial, or obsolete objects fail closed. Registry operation entrypoints
  revalidate the union, so post-open schema injection cannot bypass the
  open-time check.
- `principal_binding` and the private boundary/identity modules map trusted
  transport principals to stable users without display-name inference.
- `evidence` records privacy-classified run/feedback/authorization metadata.
  Configured Runtime always starts the recorder; content policy and
  `do_not_learn` control payload/learning use without dropping runtime facts.
  A background write failure is retained as a content-safe health issue and
  remains sticky for the process lifetime; a later write cannot repair the
  missing event.
  Only an atomically reserved new path (or an in-memory test store) may install
  its one current schema, identified by a Sylvander application ID and schema
  version. Every reopen fingerprints the exact owned `sqlite_schema` object set
  and verifies database and foreign-key integrity. Existing empty/unknown, old,
  future, unversioned, partial, foreign, or object-injected databases fail
  closed without migration, repair, or ephemeral fallback.
  Its `governance` submodule is the only persistence path for content-bearing
  events and generated artifacts: it binds one database to a tenant and
  AES-256-GCM key, enforces exact user scope, and owns retention,
  export/delete audit, and tombstones.
- `request_scoped_provider::credential_lease` acquires and renews bounded
  Provider credentials per request. Production can inject an external lease
  source through `ProviderCredentialSources`; the built-in environment/file
  adapter uses the same fail-closed generation contract.
- `credential_audit` owns the separate exact-schema
  `credential-operations.db` ledger. The live Provider request source,
  registry mutation service, and server-composed Channel credential source
  append content-safe create/acquire/renew/rotate/revoke/failure facts to it;
  no secret or secret reference enters the ledger.
- `capability_runtime` freezes disjoint Worker and Guardian registries and
  re-authorizes Runtime-derived owner scope at invocation time. The production
  `ToolInvocationGateway` freezes the exact executable tool catalog and routes
  built-ins, MCP, browser, host control, memory candidates, and registered
  extensions through that second policy check and content-safe durable audit.
  Skills are bound into the same immutable turn revision as prompt context and
  deliberately grant no execution authority. Approval gates and bounded
  artifact sinks remain typed stages of the same Agent-loop execution path.
- `guardian_runtime` and `guardian_curation` own the distinct Guardian service
  identity, durable event/run/candidate/mutation state, deterministic policy,
  idempotent canonical-memory sink, live `do_not_learn` authorization,
  credential rotation, restart catch-up, and bounded drain. The detailed
  contract is in
  [`../GUARDIAN.md`](../GUARDIAN.md).
- `execution`, `git_worktree`, and `remote_git_worktree` own location-neutral
  workspace selection plus isolated local/host-backed and SSH coding
  worktrees.
- `self_change` runs evidence-backed, isolated experiments and requires a
  distinct human merge gate.

## Critical lifecycle rules

1. Bootstrap fails closed when durable configuration, identity keys, memory
   integrity, evidence tenant/key binding, or the configured store cannot be
   validated. Session, memory, User Profile, and evidence database paths are
   normalized at this boundary: relative values resolve beneath `data_dir`,
   while empty values, `:memory:`, SQLite `file:` URIs, and existing
   directories are rejected before any store opens.
2. A channel submits every operation through the authenticated `UiService`.
   Runtime derives `user_id`, `agent_id`, session authority, workspace, and
   policy from trusted state; request payloads may request but not establish
   them.
3. Production sessions are durable. Runtime has no process-local session
   creation API or ephemeral health count; session creation must commit its
   record before Agent attachment. A persistent-session read, turn start,
   usage, assistant append, restore, or history replacement failure is a typed
   terminal error and cannot publish a successful turn.
4. Current-schema effective session configuration is persisted at creation
   with its optimistic revision, immutable Agent/Provider/Model pins,
   workspace/executor selection, and prompt manifest. Model overrides are
   provider-qualified and may shadow Agent defaults only after registry and
   capability validation. Session schema version 1 and the current registry
   component version are latest-only contracts: missing pins/manifests, a
   non-current ledger, or any non-exact schema fails closed without migration,
   repair, downgrade, or in-memory fallback. Workspace and execution-target
   changes require a new session.
5. Channel instances are supervised by stable ID with bounded restart and
   cooperative drain. One failed adapter does not erase another instance's
   session routing.
6. A writable remote coding workspace must obtain a Git worktree transaction.
   Remote non-Git mutation fails before session creation rather than falling
   back to an unjournaled host path.
7. Shutdown drains channels and Agent work, then completes the active Guardian
   pass before closing evidence and maintenance resources.

## Related documentation

- [`channel-supervision.md`](channel-supervision.md) — concrete channel
  lifecycle and restart parameters.
- [`../../docs/server-configuration.md`](../../docs/server-configuration.md)
  — configuration schema and secret references.
- [`../../docs/runtime-evidence.md`](../../docs/runtime-evidence.md) — evidence
  ledger, feedback, and self-improvement boundary.
- [`../../docs/credential-leases.md`](../../docs/credential-leases.md) —
  Provider and channel lease generation, expiry, and rotation.
- [`../CREDENTIAL_AUDIT.md`](../CREDENTIAL_AUDIT.md) — Provider/Channel
  credential-operation audit, subject isolation, and retention.
- [`../GUARDIAN.md`](../GUARDIAN.md) — Worker/Guardian capability isolation,
  curation state machine, and recovery.
- [`../../docs/module-sylvander-server.md`](../../docs/module-sylvander-server.md)
  — process composition root.
