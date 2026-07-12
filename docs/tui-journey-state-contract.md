# Sylvander TUI — Journey & State Contract

> Source of truth: `docs/sylvander-tui-ux-design.md` §30 (Primary
> End-to-End Journeys) and §31 (Focus, Shortcut, and State Ownership
> Contract).
> Implementation: `sylvander-tui` crate.
> Date: 2026-07-12

Each journey below shows the design contract from the doc, the
implementation entry point, the transitions the code drives, and the
recovery / exit states.

## 1. Start / Resume

| Phase | Design contract | Implementation |
|---|---|---|
| Entry | Launcher or sidebar | `AppState::new()` constructs the default session roster, sessions, tasks, and command catalog. `state.connected` starts `false`. |
| Workspace resolve | Mount filesystem before composer focus | `state.workspace_path` is set on `AppState::new` (placeholder). Server-emitted `SessionCreated` updates the active session id. |
| History load | Bind identity, then hydrate, then focus composer | The `ChatPanel` virtualizes messages; empty list shows only the composer. Focus precedence guarantees the composer receives keys when no modal is present. |
| Draft restore | Draft text survives modal interactions and session switching | `Composer::snapshot_draft` + `restore_draft` round-trip covers modal exits. Server-side durability is the source of truth. |
| Recovery | Missing workspace, incompatible protocol, storage read-only | `Banner::disconnected` covers the visible failure case; protocol mismatch surfaces as a `Disconnected` event with reason. |
| Exit | Composer focused with verified session identity | `AppMode::Normal` is the default end state after `SessionCreated`. |

## 2. Plan-to-Execute

| Phase | Design contract | Implementation |
|---|---|---|
| User requests planning | Composer sends `/plan` or natural-language request | `Action::SendChat` carries the prompt to the server. |
| Inspect → propose | Server emits `PlanUpdated` (TODO protocol) | `state.plan = Some(PlanState { … })`. |
| Edit | `e`/ `a`/ `d` for inline step editing | Server-side; `PlanState.changed_since_approval` flag toggles. |
| Approve | Enter on the plan card | Future modal: `PlanReviewModal` (placeholder for `state.plan.changed_since_approval`). |
| Execute | Server runs the plan, emits `ToolStarted`/`ToolResult` | Reducer handles them; `ChatPanel` renders the execution rhythm. |
| Recovery | Plan scope changed, approval withdrawn | `PlanState.changed_since_approval = true` re-prompts for approval. |
| Exit | Verified completion or explicit partial/blocked state | Final `AgentDone` or `AgentError`. |

## 3. Permission

| Phase | Design contract | Implementation |
|---|---|---|
| Tool requests capability | Server emits `ApprovalRequest` | `AppState::apply` pushes `ApprovalModal`, sets `AppMode::ApprovalPending`. |
| Review | User sees action / location / effect / scope | `ApprovalModal::render` shows action, working directory, effect summary, and scope options (Once/Session/Reject). |
| Decide | User picks a scope | `Modal::handle_key` consumes `y`/`n`/`1..n` and pushes `Action::SendApprove`. |
| Execute | Server runs the tool, emits `ToolResult` | Standard reducer path. |
| Recovery | Disconnect, timeout, changed action, remote decision | Reconnecting state freezes the approval until server state reconciles; `state.disconnect_reason` carries the surface text. |
| Exit | Audited result | `state.pending_actions` drained by `main` loop. |

## 4. Interrupt

| Phase | Design contract | Implementation |
|---|---|---|
| User requests replacement | Esc / Ctrl+C first press | `handle_key` flow: modal layer → global keys → composer. |
| Signal | Cancel agent loop | `Action::Quit` or equivalent (server-side). |
| Tool cancellation | Cancel cancellable tools, mark non-cancellable as "stopping" | Server-side. The TUI shows the interrupted tool row with `×` icon. |
| Replacement turn | New instruction begins | Composer accepts input immediately (`Composer` stays live across modal stack). |
| Recovery | Non-interruptible action, uncertain remote state | Composer remains usable; replacement is queued. |
| Exit | Replacement turn or preserved stopped state | `AgentDone` / `AgentError` clears the live area. |

## 5. Reconnect

| Phase | Design contract | Implementation |
|---|---|---|
| Transport lost | Server closes socket | `ClientEvent::Disconnected` arrives in `main`; `DomainEvent::Disconnected` updates `state.connected = false`, `state.disconnect_reason`, and pushes an `Info` message. |
| Preserve view | Transcript and composer untouched | The dispatcher's `render_too_small` and `Banner` overlays don't touch `messages` or `composer`. |
| Retry | Auto-reconnect attempt | Server socket reconnect lives in `sylvander-channel-*`; TUI surfaces state via `state.connected`. |
| Cursor reconcile | Drop duplicate deltas | The reducer keys on tool name; future work keys on `call_id`. |
| Restore live state | Resume work | `state.follow_live` flips back on `Ctrl+End` or after auto-scroll. |
| Recovery | Protocol mismatch, server crash, orphan tool | `Disconnected` reason carries diagnostic; `Banner` displays it. |
| Exit | Live session or read-only recovery | Banner remains until `Connected`. |

## 6. Fork

| Phase | Design contract | Implementation |
|---|---|---|
| User selects turn/checkpoint | `/fork` command | `CommandPalette::submit` emits a future `Action::SendChat { text: "/fork", .. }`. |
| Filesystem semantics | Same state / branch / worktree / conversation-only | Server-side. |
| Ancestry | Create new durable session | Server-side. |
| Recovery | Missing commit / worktree conflict | Server-side; `Disconnected` surfaces in the TUI. |
| Exit | New named session with explicit origin | `state.sessions` is refreshed (placeholder until server emits). |

## 7. State Contract Table

Reference: §31.2 (state ownership).

| State | Owner | Implementation | Persistence |
|---|---|---|---|
| Session id | server | `state.session_id` | durable event cursor |
| Plan | server | `state.plan` | durable plan store |
| Composer draft | client+server | `state.composer.lines` (client), server snapshot | per-session durable draft |
| Chat scroll | client | `state.chat_scroll` | per-view ephemeral |
| Modal stack | client | `state.modals` | ephemeral |
| Approval rules | server | `Action::SendApprove` | durable policy store |
| Theme | client (per-process) | `Theme::cached()` | ephemeral; resets on launch |
| Session roster | server | `state.sessions` | durable; placeholder client cache |

## 8. Acceptance Gates per Journey

| Journey | Test(s) |
|---|---|
| Start / resume | `enter_submits_chat_returns_send_action`, `composer_snapshot_survives_session_switch` |
| Plan-to-execute | `chat_renders_plan_when_present` |
| Permission | `apply_approval_request_pushes_modal`, `approval_y_sends_approve_action`, `esc_dismisses_modal_first` |
| Interrupt | `esc_quits_when_no_modal` |
| Reconnect | `apply_connected_then_disconnected`, `disconnected_state_marks_header_offline` |
| Fork | (server-side; covered by command palette catalog) |