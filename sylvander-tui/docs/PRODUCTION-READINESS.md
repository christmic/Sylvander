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
- [ ] Input submitted while working has explicit `steer`, `queue`, or
      `interrupt-and-send` semantics.
- [x] Queued prompts are visible, editable/removable, and processed in order.
- [x] Status hints only advertise implemented actions.

### Sessions

- [ ] The service reports real model, workspace, branch, and session metadata.
- [ ] Session selection loads persisted conversation history.
- [ ] Rename persists to `SessionStore`.
- [ ] Archive/delete persists and has undo-safe behavior where applicable.
- [ ] Fork creates an independent session at a selected turn boundary.
- [ ] Resume restores token/accounting and does not mix transcripts.
- [ ] Session search, recency, workspace grouping, and empty/error states work.

### Plan and background work

- [ ] Plan proposal/update/decision are typed public protocol operations.
- [ ] Plan approve, revise, reject, and cancel unblock the Agent explicitly.
- [ ] Plan current-step state is preserved and rendered correctly.
- [ ] Background task start/progress/complete/fail/cancel events are public.
- [ ] A user can inspect and cancel one task without stopping unrelated work.
- [ ] No plan/task UI is marked complete until a real Agent path activates it.

## P1 — Coding-agent expression

### Transcript and Markdown

- [ ] Render semantic Markdown instead of stripping punctuation.
- [ ] Preserve paragraphs, nested lists, quotes, links, and inline code.
- [ ] Render fenced code with language-aware styling and safe ANSI handling.
- [ ] Render tables responsively with a readable narrow fallback.
- [ ] Keep CJK, emoji, URLs, and combining characters width-correct.
- [ ] Streaming and settled Markdown retain stable geometry.

### Tool activity

- [ ] Shell rows include command, cwd, exit code, duration, stdout, and stderr.
- [ ] File reads include path, range, and language.
- [ ] Edits/writes use a real multi-file unified-diff renderer.
- [ ] Search results group matches by file with counts and line numbers.
- [ ] MCP/web/resource tools have typed presentations and safe fallback.
- [ ] Parallel calls, retries, timeouts, cancellation, and partial output render.
- [ ] Long output supports expand, focused inspection, copy, and search.
- [ ] ANSI/control sequences are sanitized and secrets are masked.

### Composer and context attachment

- [ ] `@` opens fuzzy workspace-file mention with path completion.
- [ ] Local files, images, selections, diffs, and terminal output attach safely.
- [ ] Attachment capability and size are validated against the active model.
- [ ] Attachments can be selected, removed, reordered, and restored from draft.
- [ ] Large paste payloads survive history without leaking into visible labels.
- [ ] Copy/cut/selection and external-editor workflows are complete.

### Model, permission, and context controls

- [ ] `/model` selects provider/model and reasoning effort from server truth.
- [ ] `/permissions` edits sandbox, filesystem, network, and approval policy.
- [ ] Approval supports once/session/persistent decisions where policy permits.
- [ ] `/context` reports window use, cache use, and contributing sources.
- [ ] `/compact` and automatic compaction expose progress and resulting summary.
- [ ] Cost, rate limit, retry, and model migration states are visible.
- [ ] Checkpoint, rewind, rollback, and undo have explicit safety boundaries.

### Core commands

- [ ] Session: `/resume`, `/rename`, `/fork`.
- [ ] Work: `/diff`, `/review`, `/copy`, `/mention`.
- [ ] Runtime: `/model`, `/permissions`, `/context`, `/compact`.
- [ ] Platform: `/mcp`, `/skills`, `/memory`, `/doctor`, `/config`.
- [ ] Commands support fuzzy matching, completion, aliases, recent ordering,
      availability rules, and dynamic registration.

## P2 — Platform and operations

### Extensibility

- [ ] MCP server/tool/resource health and authentication are inspectable.
- [ ] Skills show source, trust, activation, and reload state.
- [ ] Hooks show execution, output, failure, and blocking decisions.
- [ ] Extensions can contribute tools, renderers, and slash commands safely.
- [ ] Unknown protocol/tool additions degrade visibly, never silently disappear.

### Reliability and diagnostics

- [ ] Protocol versions and capabilities negotiate on connection.
- [ ] Malformed/unknown messages produce bounded diagnostic events.
- [ ] Reconnect reattaches to the active session and reconciles missed events.
- [ ] Approval, question, tool, and task timeouts have explicit recovery UI.
- [ ] `/doctor` can export a redacted diagnostic report.
- [ ] Crash-safe drafts and session state restore after terminal/server failure.
- [ ] Logs carry session, turn, request, call, and trace identifiers.

### Accessibility and configuration

- [ ] Key bindings are configurable and conflicts are detected.
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
