# TUI Production Readiness

This file is the single implementation checklist for bringing `sylvander-tui`
to production parity with mature coding-agent terminals. The visual source of
truth remains `docs/sylvander-tui-ux-design.md`; this file tracks behavior and
end-to-end delivery.

An item is complete only when all five layers are verified where applicable:

1. public protocol event or command;
2. Agent/runtime behavior;
3. service adapter behavior;
4. TUI state, input, and rendering;
5. unit, integration, snapshot, and real-terminal verification.

UI-only simulations and bracketed chat-message conventions do not count as a
completed backend feature.

## P0 — Interaction truth

### Turn control

- [x] `Esc` interrupts the active turn without terminating the Agent or TUI.
- [x] Interrupt is scoped by session and cannot affect another session/client.
- [x] Interrupt releases approval/question waits and settles pending tool rows.
- [x] The server emits an explicit interrupted terminal event.
- [x] Input submitted while working has explicit `steer`, `queue`, or
      `interrupt-and-send` semantics.
- [x] Queued prompts are visible, editable/removable, and processed in order.
- [x] Status hints only advertise implemented actions.

### Sessions

- [x] The service reports real model, workspace, branch, and session metadata.
- [x] Session selection loads persisted conversation history.
- [x] Rename persists to `SessionStore`.
- [x] Archive persists as a soft delete and `Ctrl+Z` restores the session with history intact.
- [x] Permanent delete requires typing `DELETE` and cannot be mistaken for archive.
- [x] Fork creates an independent session at the current completed turn boundary.
- [x] Resume restores durable token/accounting and replaces rather than mixes transcripts.
- [x] Session search, accurate recency, workspace grouping, and empty/error states work.

### Plan and background work

- [x] Plan proposal/update/decision are typed public protocol operations.
- [x] Plan approve, revise, reject, and cancel unblock the Agent explicitly.
- [x] Plan current-step state is preserved and rendered correctly.
- [x] Background task start/progress/complete/fail/cancel events are public.
- [x] A user can inspect and cancel one task without stopping unrelated work.
- [x] No plan/task UI is marked complete until a real Agent path activates it.

## P1 — Coding-agent expression

### Transcript and Markdown

- [x] Render semantic Markdown instead of stripping punctuation.
- [x] Preserve paragraphs, nested lists, quotes, links, and inline code.
- [x] Render fenced code with language labels, distinct styling, and safe ANSI handling.
- [x] Render tables responsively with a readable narrow fallback.
- [x] Keep CJK, emoji, URLs, and combining characters width-correct.
- [x] Streaming and settled Markdown retain stable geometry.

### Tool activity

- [x] Shell rows include command, cwd, exit code, duration, stdout, and stderr.
- [x] File reads include path, range, and language.
- [x] Edits/writes use a real multi-file unified-diff renderer.
- [x] Search results group matches by file with counts and line numbers.
- [x] MCP and unknown tools have a bounded, safe fallback presentation.
- [x] MCP/web/resource tools have dedicated typed presentations.
- [x] Independent tool calls execute concurrently and render as one deterministic batch.
- [x] Tool timeout, rejection, error, and turn cancellation settle pending rows explicitly.
- [x] LLM retries and partial tool output have typed lifecycle events and render incrementally.
- [x] Long output supports expand, focused inspection, copy, and search.
- [x] ANSI/control sequences are sanitized and secrets are masked.

### Composer and context attachment

- [x] `@` opens fuzzy workspace-file mention with path completion.
- [x] Text files and large pasted context use typed protocol attachments.
- [x] PNG/JPEG images attach through the typed protocol when the model supports vision.
- [x] Composer selections, diffs, and terminal output attach through typed context.
- [x] Attachment capability and size are validated against the active model.
- [x] Attachments can be inspected, removed, and reordered before submit.
- [x] Attachments and composer text restore from a crash-safe draft.
- [x] Large paste payloads survive submission without leaking into visible history.
- [x] Copy/cut/selection and `$VISUAL`/`$EDITOR` workflows preserve draft safety.

### Model, permission, and context controls

- [x] `/model` selects provider/model and reasoning effort from server truth.
- [x] `/permissions` edits workspace-scoped filesystem, network, and approval policy for the next turn.
- [x] Approval supports once/session/persistent exact-request decisions where policy permits.
- [x] `/context` reports provider-confirmed window use, cache use, and structural contributing sources.
- [x] `/compact` and automatic compaction expose progress, failure, and resulting summary.
- [x] Cost, rate limit, retry, and model migration states are visible.
- [x] Checkpoint, rewind, rollback, and undo have explicit safety boundaries.

### Core commands

- [x] Session: `/resume`, `/rename`, `/fork`.
- [x] Work: `/diff`, `/review`, `/mention` (with `/copy` complete).
- [x] Runtime: `/model`, `/permissions`, `/context`, `/compact`.
- [x] Platform inspection commands: `/mcp`, `/skills`, `/memory`, `/doctor`,
      `/hooks`, `/extensions`, and `/config` render server or local service
      truth.
- [x] Commands support fuzzy matching, completion, aliases, recent ordering,
      and state-derived availability rules.
- [x] Workspace/user prompt commands use dynamic registration with typed
      effects and visible collision/trust validation.

## P2 — Platform and operations

### Extensibility

- [x] MCP server/tool health, authentication state, tool count, process
      generation, and reconnect count are inspectable through fresh server
      truth.
- [x] MCP resource discovery, bounded list/read operations, resource count, and
      resource-capability health are inspectable.
- [x] Skills show the successfully activated source, Agent-home/workspace trust,
      active state, and per-turn reload behavior from fresh server truth.
- [x] Before-tool hooks show running/output/pass/failure/blocking lifecycle in
      the tool stream and expose redacted configuration through `/hooks`.
- [x] Extensions contribute tools, declarative tool presentations, and typed
      slash-command effects without receiving UI callbacks or bypassing the
      Agent tool/approval path.
- [x] Unknown protocol/tool additions degrade visibly, never silently disappear.

### Reliability and diagnostics

- [x] Protocol versions and capabilities negotiate on connection.
- [x] Malformed/unknown messages produce bounded diagnostic events.
- [x] Reconnect reattaches to the active session and reconciles missed events.
- [x] Approval, question, tool, and task timeouts have explicit recovery UI.
- [x] `/doctor` can export a redacted diagnostic report.
- [x] Crash-safe drafts and session state restore after terminal/server failure.
- [x] Logs carry session, turn, request, call, and trace identifiers.

### Accessibility and configuration

- [x] Global key bindings are configurable and conflicts are detected; text,
      decision, and interrupt safety keys remain fixed.
- [x] Optional Vim editing is complete, discoverable, and testable.
- [x] Themes validate semantic contrast and terminal color capability.
- [x] Reduced-motion and no-italic fallbacks preserve hierarchy.
- [x] Narrow, standard, and wide layouts are snapshot-verified and the compiled
      TUI reflows across 40×18, 88×24, and 132×30 PTY surfaces.
- [x] `screen-256color` and `xterm-ghostty` terminal contracts run through the
      compiled PTY flow; native Ghostty session discovery, reconciliation,
      activity, selection, and management tests pass in Xcode.
- [x] The current supported-terminal scope does not claim native tmux
      integration. PTY reflow is verified for tmux's `screen-256color` surface;
      real-process verification moves into a future tmux integration track.
- [x] SSH terminal behavior is excluded with the explicitly deferred remote
      execution track; no SSH terminal capability is advertised by this release.

## Production gates

- [x] No visible shortcut or command points to a missing effect.
- [x] No TUI-only event claims a backend feature is complete.
- [x] Public protocol changes remain UI-oriented and transport-neutral.
- [x] Agent-loop changes include cancellation, concurrency, and persistence audit.
- [x] All existing unit, E2E, and snapshot tests pass.
- [x] Real Unix-service + PTY flows:
  - [x] The compiled TUI completes Unix handshake, keyboard chat submission,
        streamed response rendering, typed approval rejection, AskUser answer,
        scoped interrupt, resize, and idle exit in a pseudo-terminal; forced
        disconnect also renegotiates and reapplies typed session history.
  - [x] Interrupt and AskUser complete against the real Agent service.
  - [x] Approval completes against the real Agent service and a rejected write
        is verified not to execute.
  - [x] Persisted SQLite session resume completes against the real Agent service
        in a PTY; canned recovery history does not satisfy this item.
- [x] Long-running and burst-stream tests show bounded memory and responsive input.
- [x] Security review:
  - [x] Workspace path scope rejects absolute paths, parent traversal, and
        symlink escape for Agent read/write/edit operations.
  - [x] Tool and diagnostic presentation removes controls and masks structured
        secrets, auth/Cookie headers, provider tokens, credential URLs, JWTs,
        and private-key blocks.
  - [x] Agent interrupt and decision routing preserves session identity.
  - [x] The Unix socket is owner-only and simultaneous clients receive live
        events only for their attached session.
  - [x] Adversarial multi-client PTY decisions, interrupt, replay, and history
        isolation are verified together.
  - [x] The real local Command executor starts each shell in an isolated process
        group. Timeout, interrupt, and dropped-future tests prove that a
        background descendant cannot outlive the owned operation.

Verification evidence (2026-07-13): `cargo test --workspace --locked` passed,
including 265 TUI unit tests, 2 TUI Unix-service E2E tests, and 46 TUI snapshots.
Four compiled-binary PTY scenarios now cover the protocol fixture and the real
`AgentRun + UnixChannel + file-backed SQLite` stack. The real-runtime scenarios
answer one Agent-owned AskUser prompt, interrupt a delayed turn, reject a write
with a typed reason and verify it never executes, then restore a persisted
transcript through `Ctrl+P` in a fresh TUI process. A two-client adversarial
scenario forces identical AskUser call IDs, proves answers and interrupts remain
session-scoped, restores a disconnected live turn from buffered events, and
audits SQLite transcripts for cross-client contamination.
Approval intent is backward compatible and transport-neutral across Unix and
WebSocket adapters. Agent tests cover scoped interrupt, concurrent tool batches,
approval cleanup, durable sessions, and runtime restore. Capacity tests cover
socket frames/events, terminal input floods, transcript retention, composer
payloads, queued prompts, session cache, and decision overlays. Credentialed
live-provider tests remain intentionally ignored and are not counted as PTY or
real-terminal verification.

## Delivery order

1. Turn control and truthful status.
2. Persistent sessions.
3. Typed Plan and background-task lifecycle.
4. Markdown, diff, and tool presentation.
5. Composer attachments and file mention.
6. Model, permission, context, and command surfaces.
7. MCP, Skills, Hooks, diagnostics, and configurable input.
8. Full production gates and release hardening.
