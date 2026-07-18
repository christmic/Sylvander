# Sylvander Production Expansion Checklist

Status: active normative checklist

Updated: 2026-07-18

This checklist replaces the former “future”, “deferred”, and “low-priority”
labels for work that is now in the active product scope. An item is complete
only when implementation, automated regression coverage, operator
documentation, and runtime evidence all exist. A type, mock-only path, or
advertised capability without an executable journey is not completion.

## E0 — Repository maintainability

- [x] Move every Rust test body out of `src/` and into the owning crate's
  `tests/` tree without widening production visibility solely for tests.
- [x] Keep one documented module boundary for every first-party crate and link
  it from the repository documentation index.
- [x] Document public invariants and non-obvious safety/lifecycle behavior in
  Rustdoc; require warning-free workspace documentation generation.
- [x] Pass format, strict all-target Clippy, full workspace tests, and locked
  release build after the relocation.

Evidence: `scripts/verify-rust-test-layout.sh` inspects nested workspaces and
its own negative fixture; `scripts/verify-docs.sh` currently accounts for all
16 first-party Cargo packages and validates maintained relative links. CI has a
warning-denied Rustdoc job. The same tracked implementation state passed root
format, strict all-target Clippy, test compilation, the complete serial
workspace suite, warning-denied Rustdoc, and the locked release build. The
nested Token9 workspace passed its independent equivalent matrix.

## E1 — Ghostty desktop host

- [x] Package the exact Sylvander TUI binary in the macOS application and
  reject a missing or non-executable helper before opening a session.
- [x] Make authenticated Agent discovery, create/list/select, PTY launch,
  reconnect, exit, and restart work through the real Unix service boundary.
- [x] Keep one TUI session per terminal surface; Ghostty alone owns the
  multi-session sidebar, lifecycle state, and focus switching.
- [x] Verify the clean ad-hoc Release bundle and optimized `ReleaseLocal`
  bundle, including universal helper/signature checks, an automated real-service
  lifecycle, and a captured operator checklist.

Evidence: Release helper embedding rejects missing/non-executable or
architecture-incomplete input and requires a universal, signed helper. The
clean ad-hoc Release bundle passed universal-helper and deep strict signature
checks, and the focused Swift suite covers session/surface ownership and
selected-child exit classification. The universal, strictly verified
`ReleaseLocal` bundle completed discovery, selection, PTY launch, server
disconnect/reconnect, child exit, and same-session restart against a real Unix
service. Captured active/inactive inspection verified a transparent host,
TrueColor identity, one portable TUI, and one native session rail. A
consistently Developer ID-signed and notarized distribution remains an external
deployment prerequisite because this environment has no Apple distribution
identity; it is not represented as incomplete product implementation.

## E2 — Complete SSH execution

- [x] Require strict host-key verification and a deployment-owned known-hosts
  file; never silently accept or learn a host key.
- [x] Reuse bounded OpenSSH control connections and expose deterministic
  health/failure behavior without leaking credentials.
- [x] Terminate the remote command process group on timeout, interruption, and
  dropped execution futures.
- [x] Implement durable SSH-native worktree leases with create, inspect,
  accept, discard, crash reconciliation, and concurrent-session isolation.
- [x] Run the executor conformance suite and one real local-SSH journey,
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
- [x] Make mutation journaling executor-neutral or explicitly require a
  worktree transaction for every remote mutable coding operation.
- [x] Complete executor, multi-instance channel, crash/restart ledger,
  self-improvement, and current-schema negative test matrices.
- [x] Run opt-in real OCI, local SSH, native tmux, provider, and configured
  channel smoke journeys where the required local service or credential is
  available; record unavailable external dependencies as deployment
  prerequisites, never as passing evidence.

Evidence: writable remote Git coding uses the SSH-native worktree transaction;
writable remote non-Git sessions fail before creation. The disposable
local-SSH journey passed execution, cancellation, restart, review, accept, and
discard. The `sylvander-improve` binary passed a real subprocess journey across
proposal review, two isolated temporary-Git experiments, successful
post-merge observation, and explicit clean rollback. The shipped server binary
also passed a real-`ServerConfig` journey with two simultaneously enabled HTTP
instances: both became ready, each accepted only its independently bound
secret, cross-instance credentials failed closed, credential operations stayed
under disjoint channel-instance audit subjects, and `SIGINT` drained both
without logging either secret. These close the self-improvement CLI and
multi-instance channel slices. The executor conformance/restart journeys,
crash-ledger recovery tests, exact-current registry matrix, and evidence-store
old/future/foreign/partial/damaged schema matrix also pass without fallback or
mutation of rejected databases. No OCI daemon, native tmux executable, live
Provider/channel credential, or Apple distribution identity was available;
those journeys are recorded in `release-closure.md` as deployment
prerequisites, not passes.

## Closure gate

All boxes above must be checked, every stale contradictory document must be
updated, generated artifacts must be clean, and `master` must be pushed only
after the full verification matrix passes. The detailed architecture remains
normative in `docs/sylvander-agent-platform.md`; this file is the executable
completion ledger.
