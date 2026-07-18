# Docs index

This is the hand-curated table of contents for the Sylvander `docs/` tree.
Each entry is one stable doc with a one-line description. Relative links
resolve from this file's location (`docs/INDEX.md`); verify with `ls docs/`
before adding new entries. `./scripts/verify-docs.sh` checks every first-party
Cargo package's boundary entry and all relative links in the maintained
documentation set.

## Server

The runtime, how it boots, and what it requires from the host.

- [server-configuration.md](server-configuration.md) — versioned TOML schema,
  startup contract, secret references, storage layout.
- [server-env.md](server-env.md) — required configuration path, tracing
  controls, and environment-backed `SecretRef` contract; environment
  variables never override the TOML schema.
- [runtime-evidence.md](runtime-evidence.md) — evidence ledger content
  policy, retention windows, recovery, and review boundary.
- [credential-leases.md](credential-leases.md) — renewable Provider and
  channel credential generations, expiry, rotation, and fail-closed rules.

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

- [chat-channel-operations.md](chat-channel-operations.md) — instance-scoped
  DingTalk, Telegram, and WeChat credential, delivery, retry, and control
  operations.
- [operations-runbook.md](operations-runbook.md) — start, stop, drain,
  log inspection, and common triage.
- [recovery-drills.md](recovery-drills.md) — restart, channel, worktree,
  memory, and release-recovery drills to run before each release.
- [release-closure.md](release-closure.md) — current release scope,
  reproducible gates, residual-risk non-claims.
- [ghostty-release-verification.md](ghostty-release-verification.md) — macOS
  Release bundle, real Unix lifecycle, transparency/TrueColor, and visual
  operator checklist.
- [performance-verification.md](performance-verification.md) —
  performance verification methodology and budget table.
- [security-verification.md](security-verification.md) — security
  verification methodology and tracked-secret scans.
- [production-expansion-checklist.md](production-expansion-checklist.md) —
  ordered production completion ledger and same-commit closure evidence.

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
- [design/README.md](design/README.md) — canonical TUI/Ghostty design entry
  point and approved brand/source asset map.

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
- [sylvander-agent/docs/approval.md](../sylvander-agent/docs/approval.md) —
  persistent approval identity, invalidation, and store operations.
- [sylvander-agent/docs/turn-context.md](../sylvander-agent/docs/turn-context.md) —
  typed prompt-layer precedence, relevance selection, provenance, and budgets.
- [sylvander-runtime/GUARDIAN.md](../sylvander-runtime/GUARDIAN.md) —
  Worker/Guardian capability separation, curation state machine, and recovery.
- [sylvander-runtime/CREDENTIAL_AUDIT.md](../sylvander-runtime/CREDENTIAL_AUDIT.md) —
  content-safe Provider/Channel credential-operation audit, retention, and
  subject isolation.
- [sylvander-channel/docs/ARCHITECTURE.md](../sylvander-channel/docs/ARCHITECTURE.md) —
  transport-neutral ingress, channel ownership, and adapter rules.
- [sylvander-llm-anthropic/docs/ARCHITECTURE.md](../sylvander-llm-anthropic/docs/ARCHITECTURE.md) —
  provider adapter, conversion, streaming, and failure ownership.
- [sylvander-tui/docs/ARCHITECTURE.md](../sylvander-tui/docs/ARCHITECTURE.md) —
  terminal client layers, service seam, and presentation state.
- [sylvander-tui/docs/CONFIGURATION.md](../sylvander-tui/docs/CONFIGURATION.md) —
  strict configuration loading, theme tokens, and environment precedence.
- [sylvander-tui/docs/INPUT-RENDERING.md](../sylvander-tui/docs/INPUT-RENDERING.md) —
  Unicode editing, cursor placement, wrapping, and terminal-cell invariants.
- [sylvander-tui/docs/INTERACTION-SCENARIOS.md](../sylvander-tui/docs/INTERACTION-SCENARIOS.md) —
  concrete chat, command, decision, profile, picker, and review interactions.
- [sylvander-tui/docs/PRODUCTION-READINESS.md](../sylvander-tui/docs/PRODUCTION-READINESS.md) —
  implemented TUI capability ledger and verification commands.
- [sylvander-tui/docs/SECURITY.md](../sylvander-tui/docs/SECURITY.md) —
  client trust boundary, redaction, terminal sanitization, and clipboard rules.
- [module-sylvander-protocol.md](module-sylvander-protocol.md) —
  latest wire schema, identifiers, negotiation, and generated contracts.
- [module-sylvander-llm-core.md](module-sylvander-llm-core.md) —
  provider-neutral model requests, streaming, capabilities, and errors.
- [module-sylvander-server.md](module-sylvander-server.md) —
  process composition root, configuration handoff, and shutdown ownership.
- [module-sylvander-channel-dingtalk.md](module-sylvander-channel-dingtalk.md) —
  DingTalk ingress authentication, replay control, delivery, and supervision.
- [module-sylvander-channel-http.md](module-sylvander-channel-http.md) —
  bounded HTTP/SSE debug ingress and operational endpoints.
- [module-sylvander-channel-telegram.md](module-sylvander-channel-telegram.md) —
  Telegram webhook authentication, replay control, and outbound delivery.
- [module-sylvander-channel-unix.md](module-sylvander-channel-unix.md) —
  authenticated local UI protocol over Unix sockets.
- [module-sylvander-channel-ws.md](module-sylvander-channel-ws.md) —
  WebSocket UI transport, handshake, identity, and flow control.
- [module-sylvander-channel-wechat.md](module-sylvander-channel-wechat.md) —
  WeChat Work callback encryption/replay, renewable credentials, controls, and
  active-API delivery.
- [token9-contracts/docs/ARCHITECTURE.md](../sylvander-token9/token9-contracts/docs/ARCHITECTURE.md) —
  token9 management/read DTO ownership and generated-client ABI boundary.
- [token9-server/docs/ARCHITECTURE.md](../sylvander-token9/token9-server/docs/ARCHITECTURE.md) —
  separately built local LLM gateway, routing, persistence, and deployment
  trust boundary.

## User manual

- [user-manual.md](user-manual.md) — install, first run, daily Agent
  usage, terminal UI tour, configuration, workspaces, and operational limits.

## Developer manual

- [developer-manual.md](developer-manual.md) — repo layout, toolchain,
  build/test/lint, CI tour, how-to guides for adding channels, tools,
  executors, MCP, Skills, identity changes, and release drills.
- [rust-test-layout.md](rust-test-layout.md) — production/test directory
  contract, white-box bridge rule, and migration verification.
