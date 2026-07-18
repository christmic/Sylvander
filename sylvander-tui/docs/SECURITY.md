# TUI Security Boundary

This document records the security contract of `sylvander-tui`. The TUI is a
terminal client, not a sandbox: the Agent owns tool capability enforcement,
workspace scope, approval policy, process lifecycle, and durable permissions.
The TUI owns safe input retention, transport framing, decision intent, and
bounded/redacted presentation.

## Trust boundaries

- Panels and Modals are pure renderers and cannot execute tools or read secrets.
- `AgentService` translates typed actions; it never interprets or broadens an
  approval, permission profile, session ID, or workspace path.
- The Agent derives filesystem roots from server-owned session metadata. A path
  displayed or submitted by the TUI is not an authorization grant.
- An unknown protocol version or envelope fails closed with a bounded diagnostic
  and cannot mutate UI state. An unknown tool name within the negotiated current
  protocol remains visible through the bounded generic tool renderer, with
  terminal controls removed and sensitive fields redacted.
- Diagnostic exports contain compacted paths and never include prompt bodies,
  tool input/output, environment variables, credentials, or socket frames.

## Verified controls

The 2026-07-13 audit verified these controls against implementation and tests:

- Read rejects canonical paths and symlinks that leave the workspace.
- Write and Edit resolve non-empty relative paths through `WorkspaceJournal`,
  rejecting absolute paths, parent traversal, and symlink hops.
- Permission profiles construct a workspace-scoped `ToolContext`; the TUI only
  selects profiles advertised by the service.
- Interrupt routing is keyed by session, so one session cannot cancel another.
- The Unix Agent socket is forced to owner-only `0600`; startup fails closed if
  those permissions cannot be applied. Two-client socket tests verify live
  events remain attached to their own session.
- Approval rejection reasons remain typed and transport-neutral through Unix
  and WebSocket adapters; the Agent remains the decision authority.
- Tool input/output strips terminal controls and redacts structured sensitive
  keys, known provider tokens, authentication/Cookie headers, secret
  assignments, credential-bearing URLs, JWTs, and PEM private-key blocks.
- Retained frames, events, transcript entries, drafts, attachments, queues,
  session summaries, and modal backlogs all have explicit memory ceilings.

## Release security status

- `CommandTool` executes through the workspace executor. Timeout and dropped
  futures terminate the complete local process group; the regression tests
  verify that descendants do not survive.
- The compiled real-runtime PTY journey exercises colliding multi-client
  decisions, interrupts, replay, and history and proves session isolation.
- Credentialed live-provider tests remain opt-in deployment evidence. They do
  not replace deterministic provider contracts, redaction tests, or the
  same-commit release gate.

There is no open TUI implementation blocker in this document. A deployment
that enables a live provider or external channel must still run its
credentialed smoke journey and inspect redacted logs in that environment.
