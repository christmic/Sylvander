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
- Unknown protocol and tool data remains visible through bounded fallbacks, with
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
- Approval rejection reasons remain typed and transport-neutral through Unix
  and WebSocket adapters; the Agent remains the decision authority.
- Tool input/output strips terminal controls and redacts structured sensitive
  keys, known provider tokens, authentication/Cookie headers, secret
  assignments, credential-bearing URLs, JWTs, and PEM private-key blocks.
- Retained frames, events, transcript entries, drafts, attachments, queues,
  session summaries, and modal backlogs all have explicit memory ceilings.

## Open release blockers

- Sylvander Agent does not currently expose a shell/exec tool. Process-group
  termination and descendant cleanup therefore cannot be verified; a visual
  shell renderer is not evidence of safe shell cancellation.
- Adversarial two-client PTY/socket verification must prove that decisions,
  interrupts, replay, and session history never cross session ownership.
- Credentialed live-provider tests remain opt-in and are not security evidence
  unless they run in the release environment with redacted logs inspected.

The production security gate stays open until the applicable blockers above
have executable tests. Do not mark an unavailable backend capability complete
from TUI fixtures or snapshots.
