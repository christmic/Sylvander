# Docs index

This is the hand-curated table of contents for the Sylvander `docs/` tree.
Each entry is one stable doc with a one-line description. Relative links
resolve from this file's location (`docs/INDEX.md`); verify with `ls docs/`
before adding new entries.

## Server

The runtime, how it boots, and what it requires from the host.

- [server-configuration.md](server-configuration.md) — versioned TOML schema,
  startup contract, secret references, storage layout.
- [server-env.md](server-env.md) — environment variables the server
  consults and how they override or supplement the TOML config.
- [runtime-evidence.md](runtime-evidence.md) — evidence ledger content
  policy, retention windows, recovery, and review boundary.

## Protocols

Wire contracts the server implements, audited as the latest interface.

- [boundary-authorization.md](boundary-authorization.md) — channel
  authentication, principal mapping, Agent access policy, audit.
- [identity-binding-protocol.md](identity-binding-protocol.md) — stable
  user identity binding via link codes and trusted issuers.
- [user-profile-protocol.md](user-profile-protocol.md) — global user
  profile envelope, schema, deletion tombstones, capability advert.

## Operations

Day-2 operator docs for keeping Sylvander production-ready.

- [operations-runbook.md](operations-runbook.md) — start, stop, drain,
  log inspection, and common triage.
- [recovery-drills.md](recovery-drills.md) — restart, channel, worktree,
  memory, and release-recovery drills to run before each release.
- [release-closure.md](release-closure.md) — local-first release scope,
  reproducible gates, residual-risk non-claims.
- [performance-verification.md](performance-verification.md) —
  performance verification methodology and budget table.
- [security-verification.md](security-verification.md) — security
  verification methodology and tracked-secret scans.
- [production-expansion-checklist.md](production-expansion-checklist.md) —
  ordered execution SSOT for post-local-first and lower-priority capabilities.

## Architecture

Design notes for the platform, terminal substrate, and brand.

- [sylvander-agent-platform.md](sylvander-agent-platform.md) — agent
  loop, tool/skill/MCP surface, supervisor layout.
- [sylvander-ghostty-architecture.md](sylvander-ghostty-architecture.md) —
  Zig/GTK/macOS terminal substrate fork and how Sylvander embeds it.
- [sylvander-tui-ux-design.md](sylvander-tui-ux-design.md) — terminal
  UI composition, focus, responsive dock, decision surfaces.
- [sylvander-brand-system.md](sylvander-brand-system.md) — brand
  system, design tokens, visual assets catalog.

## Module references

Per-crate documentation linked from the runtime and agent sub-projects. The
top-level files below are the stable public references for smaller adapters;
the core crates keep their detailed ownership documents beside their source.
When a crate changes its public shape, update the closest module-owned document
and this index in the same change.

- [sylvander-runtime/docs/channel-supervision.md](../sylvander-runtime/docs/channel-supervision.md) —
  bounded restart and lifecycle contract for channel instances.
- [sylvander-runtime/docs/ARCHITECTURE.md](../sylvander-runtime/docs/ARCHITECTURE.md) —
  Runtime composition, trusted ownership, registry, and worktree boundaries.
- [sylvander-agent/docs/ARCHITECTURE.md](../sylvander-agent/docs/ARCHITECTURE.md) —
  per-turn execution, tools, memory, MCP, Skills, and extension rules.
- [sylvander-agent/docs/workspace-execution.md](../sylvander-agent/docs/workspace-execution.md) —
  executor dispatch across local, SSH, container, and sandbox targets.
- [sylvander-agent/docs/mcp.md](../sylvander-agent/docs/mcp.md) —
  MCP runtime lifecycle, health, reconnection contract.
- [sylvander-agent/docs/skills.md](../sylvander-agent/docs/skills.md) —
  Skill package discovery, precedence, isolation, and the per-turn budget.
- [sylvander-channel/docs/ARCHITECTURE.md](../sylvander-channel/docs/ARCHITECTURE.md) —
  transport-neutral ingress, channel ownership, and adapter rules.
- [sylvander-llm-anthropic/docs/ARCHITECTURE.md](../sylvander-llm-anthropic/docs/ARCHITECTURE.md) —
  provider adapter, conversion, streaming, and failure ownership.
- [sylvander-tui/docs/ARCHITECTURE.md](../sylvander-tui/docs/ARCHITECTURE.md) —
  terminal client layers, service seam, and presentation state.

## User manual

- [user-manual.md](user-manual.md) — install, first run, daily Agent
  usage, terminal UI tour, configuration, workspaces, and operational limits.

## Developer manual

- [developer-manual.md](developer-manual.md) — repo layout, toolchain,
  build/test/lint, CI tour, how-to guides for adding channels, tools,
  executors, MCP, Skills, identity changes, and release drills.
- [rust-test-layout.md](rust-test-layout.md) — production/test directory
  contract, white-box bridge rule, and migration verification.
