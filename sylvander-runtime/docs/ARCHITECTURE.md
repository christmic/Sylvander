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
  -> authenticated ChannelHost
  -> channel supervisor
  -> AgentRunEngine
```

The server binary supplies configuration and process lifetime only. Individual
channels own their native protocol adapters; the Agent crate owns one run;
Runtime owns the binding between them.

## Physical source layout

The internal source tree uses concrete Runtime responsibilities as directory
names. It does not introduce generic top-level `domain`, `application`,
`infrastructure`, or `ports` buckets: those names hide what the code actually
does at this composition layer.

```text
src/
├── lib.rs                 # public facade and compatibility exports only
├── runtime/               # boot, lifecycle, ChannelHost, health, shutdown
├── agent/                 # definition, run, supervision, approval, prompt
├── session/               # context, authenticated boundary, identity binding
├── registry/              # Agent/model/provider revision governance
├── provider/              # catalogs and request-scoped provider routing
├── credential/            # secret resolution and content-safe audit
├── guardian/              # Guardian runtime and curation
├── workspace/             # coding, local/remote worktrees, self-change
├── execution/             # target selection and concrete executors
├── storage/               # durable repositories and health composition
├── evidence/              # evidence records and governed artifacts
├── mcp/                   # Session MCP lifecycle and stdio transport
├── observability/         # typed Runtime lifecycle facts
└── config/                # validated server configuration
```

`lib.rs` may preserve stable external names such as `agent_run`,
`git_worktree`, or `credential_audit` through re-exports. Internal production
code imports the owning physical path (`agent::run`, `workspace::local`,
`credential::audit`) so the repository layout remains the source of truth.
`runtime/mod.rs` contains the process-level orchestration that previously
occupied the crate root; it is not a second public facade.

More precisely, Runtime owns the product Session and constructs one immutable
Agent turn from its durable snapshot. Agent owns only the bounded inference and
tool state machine. The normative target dependency graph and Session
vocabulary are in
[`../../docs/agent-runtime-api-boundaries.md`](../../docs/agent-runtime-api-boundaries.md).
During migration, `AgentRun`, `AgentRunEngine`, Session persistence, bus
publication, and public event mapping move from `sylvander-agent` into this
crate without retaining a second production path.

## Module responsibilities

- `config` validates latest-version configuration, resolves declarative
  references, and rejects unsupported legacy shapes rather than guessing.
- `composition` builds configured Agent revisions, default tools, prompt
  layers, and selected provider adapters from Runtime-owned inputs.
- `registry` makes Agent/model revisions immutable for a run and exposes
  administrator-facing updates through explicit
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
- `session` maps trusted transport principals to stable users, owns the
  authenticated boundary and identity binding, and keeps Session context
  separate from durable storage without display-name inference.
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
- `provider::request_scoped::credential_lease` acquires and renews bounded
  Provider credentials per request. Production can inject an external lease
  source through `ProviderCredentialSources`; the built-in environment/file
  adapter uses the same fail-closed generation contract.
- `credential::audit` owns the separate exact-schema
  `credential-operations.db` ledger. The live Provider request source,
  registry mutation service, and server-composed Channel credential source
  append content-safe create/acquire/renew/rotate/revoke/failure facts to it;
  no secret or secret reference enters the ledger.
- `storage` is the closed persistence composition root. It owns the Session
  commit authority and relationship-memory backend, and aggregates Agent
  Registry, User Profile, Evidence, credential-audit, and both Guardian-store
  health planes. It retains concrete health
  probes only inside Runtime, and exposes one content-free operational view.
  Production health revalidates each live schema plus SQLite integrity;
  Session and Registry also check owned foreign keys, Registry checks its
  shared namespace and current ledger, and protected memory verifies its
  independent authenticated anchor. A degraded component makes Runtime
  unready without disclosing paths, row data, database errors, or key/anchor
  material. The Agent receives only cloned provider-neutral ports and cannot
  select, inspect, or probe a backend.
  Evidence database degradation is distinct from the recorder's sticky
  asynchronous write-failure signal: the former is a `Storage` issue, while
  the latter remains `EvidenceRecorder` because a later successful write
  cannot reconstruct a missing fact.
- `capability_runtime` freezes disjoint Worker and Guardian registries and
  re-authorizes Runtime-derived owner scope at invocation time. The production
  `ToolInvocationGateway` freezes the exact executable tool catalog and routes
  built-ins, MCP, browser, host control, memory candidates, and registered
  extensions through that second policy check and content-safe durable audit.
  Skills are bound into the same immutable turn revision as prompt context and
  deliberately grant no execution authority. Approval gates and bounded
  artifact sinks remain typed stages of the same Agent-loop execution path.
  Runtime composes a separate, cache-stable system-instruction block from the
  exact frozen tool registry. Restricted background work recomputes that block
  after reducing its catalog to stable read-tool names; it never inherits
  guidance for tools it cannot invoke.
- `guardian::runtime` and `guardian::curation` own the distinct Guardian service
  identity, durable event/run/candidate/mutation state, deterministic policy,
  idempotent canonical-memory sink, live `do_not_learn` authorization,
  credential rotation, restart catch-up, and bounded drain. The detailed
  contract is in
  [`../GUARDIAN.md`](../GUARDIAN.md).
  Curation and canonical storage are latest-only exact-schema databases.
  RuntimeStorage receives a health-only clone of their store handles rather
  than the Guardian supervisor. Consequently a schema/page failure contributes
  `Storage`, while a supervisor-loop failure contributes
  `GuardianSupervisor`; neither can mask or impersonate the other.
- `execution` and `workspace::{coding,local,remote}` own location-neutral
  workspace selection, the concrete host-local/SSH/OCI adapters, and isolated
  local/host-backed and SSH coding worktrees. The local adapter enforces path,
  output, deadline, and process-tree cancellation bounds but truthfully
  reports no sandbox isolation. Runtime boot builds one immutable exact-target
  execution service and reuses it for initial and lazy Agent revisions;
  unknown targets never inherit the built-in `local` adapter. Operational
  diagnostics expose only target ID, adapter kind, health state, probe count,
  and static isolation facts. One owned worker performs bounded SSH/OCI probes,
  marks degradation unready, retries, and joins during shutdown.
- `storage::memory` owns the closed SQLite relationship-memory backend,
  authenticated file/HTTP integrity anchors, finite retention, evidence
  checkpoints, backup rotation, and offline restore. Agent owns only the
  provider-neutral memory values, validation rules, and `MemoryStore` port;
  Runtime selects, opens, maintains, and injects the concrete store.
- `storage::RuntimeStorage` is the crate-private composition root for durable
  repositories. It closes public access to the selected Session,
  relationship-memory, and encrypted turn-artifact authorities and aggregates
  live integrity for nine
  production stores. Session schema v2 makes turn lifecycle
  authoritative: admission commits user input, configuration, and `running`;
  successful completion commits assistant output and `completed` in one
  transaction. The latest-only v3 schema adds content-free tool lifecycle:
  one start and one terminal per call, no successful turn with a running call,
  and atomic abandonment with failed/interrupted turns. Turn-bound artifacts
  use the encrypted governed store, an
  independent health component, and opaque locators. Retrieval,
  cross-domain transactions, and unified backup remain incomplete; see
  [`application-services.md`](application-services.md) for exact status.
- `observability` is the closed typed lifecycle recorder. Its first slice
  covers authorized chat admission, message-bus dispatch, turn terminals,
  model retries, tool terminals, and durable Session operations with
  content-free counters and structured facts in the Runtime operational
  snapshot. One recorder is shared by initial and lazy Agent revisions. The
  durable Session turn is the product terminal authority; Evidence remains a
  separate asynchronous governance projection. Metric durability and sink
  health remain incomplete.
- `mcp` owns authenticated Session bindings, secret resolution, persistent
  sandbox selection, server lifecycles, and governed results. `mcp::stdio`
  implements JSON-RPC, discovery, cancellation, health, and reconnection over
  Runtime's persistent-process port; production code has no host-spawn path.
  Discovered tools are composed through Agent's neutral Session-extension
  boundary and frozen with their exact authorization gateway for each turn.
- `workspace::self_change` runs evidence-backed, isolated experiments and
  requires a distinct human merge gate.

## Critical lifecycle rules

1. Bootstrap fails closed when durable configuration, identity keys, memory
   integrity, evidence tenant/key binding, or the configured store cannot be
   validated. Session, memory, User Profile, and evidence database paths are
   normalized at this boundary: relative values resolve beneath `data_dir`,
   while empty values, `:memory:`, SQLite `file:` URIs, and existing
   directories are rejected before any store opens.
2. A channel submits every operation through the authenticated `ChannelHost`.
   Runtime derives `user_id`, `agent_id`, session authority, workspace, and
   policy from trusted state; request payloads may request but not establish
   them.
3. Production sessions are durable. Runtime has no process-local session
   creation API or ephemeral health count; session creation must commit its
   record before Agent attachment. A persistent-session read, turn start,
   usage, turn completion, restore, or history replacement failure is a typed
   terminal error and cannot publish a successful turn. Assistant output and
   the completed terminal commit atomically before public `Done`.
4. Current-schema effective session configuration is persisted at creation
   with its optimistic revision, immutable Agent/Provider/Model pins,
   workspace/executor selection, and prompt manifest. Model overrides are
   provider-qualified and may shadow Agent defaults only after registry and
   capability validation. Session schema version 2 and the current registry
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

- [`application-services.md`](application-services.md) — Session supervision,
  execution/sandbox ownership, unified storage, and built-in observability.
- [`mcp.md`](mcp.md) — Session-owned MCP lifecycle, persistent process sandbox,
  immutable tool snapshots, protocol bounds, cancellation, and current
  transport limits.
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
