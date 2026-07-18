# Sylvander Production Expansion Checklist

Status: active normative checklist

Updated: 2026-07-18

This checklist replaces the former “future”, “deferred”, and “low-priority”
labels for work that is now in the active product scope. An item is complete
only when implementation, automated regression coverage, operator
documentation, and runtime evidence all exist. A type, mock-only path, or
advertised capability without an executable journey is not completion.

## E0 — Repository maintainability

- [ ] Move every Rust test body out of `src/` and into the owning crate's
  `tests/` tree without widening production visibility solely for tests.
- [ ] Keep one documented module boundary for every first-party crate and link
  it from the repository documentation index.
- [ ] Document public invariants and non-obvious safety/lifecycle behavior in
  Rustdoc; require warning-free workspace documentation generation.
- [ ] Pass format, strict all-target Clippy, full workspace tests, and locked
  release build after the relocation.

## E1 — Ghostty desktop host

- [ ] Package the exact Sylvander TUI binary in the macOS application and
  reject a missing or non-executable helper before opening a session.
- [ ] Make authenticated Agent discovery, create/list/select, PTY launch,
  reconnect, exit, and restart work through the real Unix service boundary.
- [ ] Keep one TUI session per terminal surface; Ghostty alone owns the
  multi-session sidebar, lifecycle state, and focus switching.
- [ ] Verify a signed application bundle from a clean build directory with an
  automated launch journey and a captured operator checklist.

## E2 — Complete SSH execution

- [x] Require strict host-key verification and a deployment-owned known-hosts
  file; never silently accept or learn a host key.
- [x] Reuse bounded OpenSSH control connections and expose deterministic
  health/failure behavior without leaking credentials.
- [x] Terminate the remote command process group on timeout, interruption, and
  dropped execution futures.
- [x] Implement durable SSH-native worktree leases with create, inspect,
  accept, discard, crash reconciliation, and concurrent-session isolation.
- [ ] Run the executor conformance suite and one real local-SSH journey,
  including restart and worktree review.

## E3 — Worker/Guardian separation

- [x] Give Worker and Guardian actors independent registries and immutable
  per-run capability snapshots. A Worker cannot discover Guardian schemas.
- [x] Derive every memory/workspace owner from Runtime context and enforce a
  second fail-closed policy check at invocation time.
- [x] Run Guardian with a distinct service identity and strict capability
  allowlist that excludes terminal, browser, host control, and arbitrary MCP.
- [x] Prove forged routes, hidden-tool discovery, and cross-owner access fail
  without revealing protected existence.

## E4 — Guardian curation

- [x] Persist idempotent outbox events and retryable `CuratorRun` state
  independently from conversational sessions.
- [x] Persist typed candidates with evidence, scope, confidence, sensitivity,
  consent state, retention, deduplication, and conflict state.
- [x] Implement extract, classify, reconcile, confirm, policy-check, commit,
  correct, decay, and forget transitions with durable audit.
- [x] Require deterministic policy authorization for every canonical-memory or
  user-profile mutation; model classification alone is never authority.

## E5 — Context and capability runtime

- [x] Compose typed safety, Agent, User Profile, relationship memory, workspace
  knowledge, and session layers with provenance, digests, precedence, and
  explicit per-layer token budgets.
- [x] Retrieve dynamic relationship/workspace context by relevance and remove
  expired or superseded facts without dumping complete stores into a prompt.
- [x] Bind Skills into immutable prompt context without executable authority,
  and route built-ins, MCP, browser, host control, memory candidates, and
  registered extensions through one actor-aware router, policy gateway,
  approval contract, artifact store, and content-safe audit record.
- [x] Scope persistent approvals by stable identity, Agent, policy revision,
  capability revision, operation, and resource fingerprint.

## E6 — Governance and remaining production evidence

- [x] Apply explicit data classification, redaction, retention, deletion,
  export, encryption-at-rest configuration, and tenant isolation to run
  evidence and generated artifacts.
- [x] Support renewable external secret leases and uniform credential
  generation rotation for providers and every channel instance.
- [ ] Make mutation journaling executor-neutral or explicitly require a
  worktree transaction for every remote mutable coding operation.
- [ ] Complete executor, multi-instance channel, crash/restart ledger,
  self-improvement, and current-schema negative test matrices.
- [ ] Run opt-in real OCI, local SSH, native tmux, provider, and configured
  channel smoke journeys where the required local service or credential is
  available; record unavailable external dependencies as deployment
  prerequisites, never as passing evidence.

## Closure gate

All boxes above must be checked, every stale contradictory document must be
updated, generated artifacts must be clean, and `master` must be pushed only
after the full verification matrix passes. The detailed architecture remains
normative in `docs/sylvander-agent-platform.md`; this file is the executable
completion ledger.
