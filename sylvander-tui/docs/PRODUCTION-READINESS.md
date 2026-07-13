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
      and `/config` render server or local service truth.
- [x] Commands support fuzzy matching, completion, aliases, recent ordering,
      and state-derived availability rules.
- [x] Workspace/user prompt commands use dynamic registration with typed
      effects and visible collision/trust validation.
- [ ] Runtime extensions can register non-prompt slash effects through an
      Agent-owned dispatcher.

## P2 — Platform and operations

### Extensibility

- [ ] MCP server/tool/resource health and authentication are inspectable.
- [ ] Skills show source, trust, activation, and reload state.
- [ ] Hooks show execution, output, failure, and blocking decisions.
- [ ] Extensions can contribute tools, renderers, and slash commands safely.
- [ ] Unknown protocol/tool additions degrade visibly, never silently disappear.

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
- [ ] Optional Vim editing is complete, discoverable, and testable.
- [ ] Themes validate semantic contrast and terminal color capability.
- [ ] Reduced-motion and no-italic fallbacks preserve hierarchy.
- [ ] Narrow, standard, wide, tmux, SSH, and Ghostty surfaces are verified.

## Production gates

- [ ] No visible shortcut or command points to a missing effect.
- [ ] No TUI-only event claims a backend feature is complete.
- [ ] Public protocol changes remain UI-oriented and transport-neutral.
- [ ] Agent-loop changes include cancellation, concurrency, and persistence audit.
- [ ] All existing unit, E2E, and snapshot tests pass.
- [ ] Real Unix-service + PTY flows cover chat, interrupt, approval, AskUser,
      reconnect, session resume, and resize.
- [ ] Long-running and burst-stream tests show bounded memory and responsive input.
- [ ] Security review covers path scope, shell cancellation, secret masking, and
      multi-client/session isolation.

## Delivery order

1. Turn control and truthful status.
2. Persistent sessions.
3. Typed Plan and background-task lifecycle.
4. Markdown, diff, and tool presentation.
5. Composer attachments and file mention.
6. Model, permission, context, and command surfaces.
7. MCP, Skills, Hooks, diagnostics, and configurable input.
8. Full production gates and release hardening.
