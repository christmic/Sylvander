# `sylvander-runtime` architecture

`sylvander-runtime` is the composition and ownership layer for the Sylvander
server. It turns versioned configuration into durable stores, Agent revisions,
provider routing, channel instances, and auditable control operations. It is
the only layer that may establish trusted execution identity from an external
transport.

## Composition graph

```text
ServerConfig + SecretResolver
  -> Runtime::boot_config
  -> durable stores (sessions, memory, evidence, identity bindings)
  -> Agent registry + provider registry
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
  revision checks.
- `principal_binding` and the private boundary/identity modules map trusted
  transport principals to stable users without display-name inference.
- `evidence` records privacy-classified run/feedback/authorization evidence.
- `execution` and `git_worktree` own workspace selection and isolated local
  coding worktrees.
- `self_change` runs evidence-backed, isolated experiments and requires a
  distinct human merge gate.

## Critical lifecycle rules

1. Bootstrap fails closed when durable configuration, identity keys, memory
   integrity, or the configured store cannot be validated.
2. A channel submits every operation through the authenticated `UiService`.
   Runtime derives `user_id`, `agent_id`, session authority, workspace, and
   policy from trusted state; request payloads may request but not establish
   them.
3. Session configuration is persisted at creation. Model overrides may shadow
   Agent defaults only after registry and capability validation; workspace and
   execution-target changes require a new session.
4. Channel instances are supervised by stable ID with bounded restart and
   cooperative drain. One failed adapter does not erase another instance's
   session routing.
5. Shutdown drains channels and Agent work before closing durable resources.

## Related documentation

- [`channel-supervision.md`](channel-supervision.md) — concrete channel
  lifecycle and restart parameters.
- [`../../docs/server-configuration.md`](../../docs/server-configuration.md)
  — configuration schema and secret references.
- [`../../docs/runtime-evidence.md`](../../docs/runtime-evidence.md) — evidence
  ledger, feedback, and self-improvement boundary.
- [`../../docs/module-sylvander-server.md`](../../docs/module-sylvander-server.md)
  — process composition root.
