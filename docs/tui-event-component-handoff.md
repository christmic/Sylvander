# Sylvander TUI — Event-to-Component Handoff

> Source of truth: `docs/sylvander-tui-ux-design.md` §33 (Event-to-Component
> Handoff) and §34 (Design QA Scenarios).
> Implementation: `sylvander-tui` crate.
> Date: 2026-07-12

This document mirrors the design doc's event table into implementation
artefacts. Each protocol event maps to a `DomainEvent` reducer (in
`src/event.rs`), the reducer mutates `AppState`, and the rendering
panels in `src/panel/*` read the new state and draw into the buffer.

## 1. Wire → DomainEvent → State → Renderer

| Wire (`ServerMsg`) | `DomainEvent`              | State mutation                                                                       | Renderer effect                                          |
|--------------------|----------------------------|--------------------------------------------------------------------------------------|----------------------------------------------------------|
| `SessionCreated`   | `SessionCreated`           | `state.session_id = Some(id)`                                                        | `HeaderPanel` shows new session id; `ChatPanel` clears.  |
| `TextDelta`        | `TextChunk { delta }`      | Append to `state.streaming`                                                          | `ChatPanel` renders one live block, bottom-aligned.      |
| `ThinkingDelta`    | `ThinkingChunk { delta }`  | Append to `state.streaming_thinking`                                                 | `ChatPanel` renders italic muted block above text.       |
| `ToolCall`         | `ToolStarted`              | Push `ChatMessage::ToolCall { status: Pending, .. }`                                 | `ChatPanel` renders row with `◐` running icon.           |
| `ToolResult`       | `ToolFinished`            | Flip matching pending row to `Done`/`Error`, push `ToolResult`                       | `ChatPanel` renders `✓`/`×` evidence line.               |
| `Done`             | `AgentDone { final_text }` | Promote `streaming` (or use `final_text`) into `messages`, clear streaming            | `ChatPanel` settles one agent turn; streaming block gone.|
| `Error`            | `AgentError { message }`   | Push `ChatMessage::Info`, clear streaming buffers                                    | `ChatPanel` shows error inline; `HeaderPanel` keeps layout.|
| `ApprovalRequest`  | `ApprovalRequested`        | Push `ApprovalModal`, set `AppMode::ApprovalPending`                                 | `ApprovalModal` overlays; `HelpPanel` swaps hints.       |
| (no protocol yet)  | `PlanUpdated` (TODO)       | Set `state.plan = Some(...)`; mark `changed_since_approval` if step set changes     | `ChatPanel` renders inline plan region (steps + icons).  |
| (no protocol yet)  | `AskUser` (TODO)           | Push `AskUserModal`, set `AppMode::AskPending`                                       | `AskUserModal` overlays with single/multi/free variants. |
| `IterationStart`   | (none — heartbeat)         | Update `state.tool_count`                                                             | `HeaderPanel` shows tool count; `InputPanel` status line.|
| `Pong`             | (none — heartbeat)         | No-op                                                                                | n/a                                                      |

## 2. Component State Machines

All component state machines use stable IDs and idempotent updates:

- **Tool rows.** The reducer keys on `tool_name` to flip `ToolStatus::Pending`
  → `Done`/`Error`. Future work: key on `call_id` so multiple in-flight calls
  of the same tool name do not collide.
- **Plan.** `PlanStep.id` is the stable ID. `push_plan_lines` matches by id
  across updates; new ids append, removed ids drop, status changes flip the
  icon.
- **Session roster.** `SessionEntry.id` is the stable ID. `state.sessions`
  is rebuilt on server refresh; selected id persists across redraws.

## 3. Keyboard Precedence

Reference: §31.1 (keyboard ownership), §28.1 (interaction precedence).

| Rank | Layer | Key behavior                                  | Implementation                               |
|------|-------|-----------------------------------------------|----------------------------------------------|
| 1    | Destructive approval | `y`/`n`/`1..n` confirm; `Esc` denies. | `modal/approval.rs`                            |
| 2    | AskUser             | `1..n` for single, space+enter for multi, type+enter for free | `modal/ask_user.rs` |
| 3    | Session switcher    | type filters; ↑/↓ moves; ↵ opens; `Esc` closes | `modal/session_switcher.rs`     |
| 4    | Task overlay        | ↑/↓ moves; `Esc` closes                       | `modal/task_overlay.rs`                       |
| 5    | Command palette     | type filters; ↑/↓ moves; ↵ runs; `Esc` closes | `modal/command_palette.rs`      |
| 6    | Help overlay        | `?`/`Esc` closes                              | `modal/help_overlay.rs`                       |
| 7    | Composer (default)  | text + Enter (send) / Shift+Enter (newline)   | `panel/input.rs`, `ui/composer.rs`            |
| 8    | Transcript scroll   | `PageUp`/`PageDown`/`Ctrl+End`                | `app.rs::handle_key`                          |

`Ctrl+C` from any layer sets `should_quit` (with the design's "first
press interrupts, second press exits" semantics pending modal integration).
`Esc` from any layer first dismisses the topmost modal, then quits.

## 4. State Ownership

Reference: §31.2 (state ownership).

| State | Owner | Persisted? | Reconciled how |
|---|---|---|---|
| Agent execution & tool lifecycle | server | durable event cursor | client resumes from last acknowledged cursor |
| Session history, plan, checkpoint | server | durable store | server is authoritative |
| Draft content | server-backed, client-edited | per-session durable draft | `Composer::snapshot_draft` + `restore_draft` |
| Sidebar order/collapse | Ghostty window (out of scope) | per-window | local |
| Transcript scroll/expansion | TUI view | per-view session state | `state.chat_scroll` + `state.expanded_tools` |
| Permission rules & audit | server | durable policy | broadcast to all clients |
| IME composition | OS input client | ephemeral | never persisted as draft text |

## 5. Acceptance Gates (Design §34)

| Scenario | Implementation test |
|---|---|
| Given an active IME composition, when Enter is pressed, then the candidate commits and no prompt is sent. | `composer::tests::ime_enter_commits_candidate_does_not_send` |
| Given a user reading old history, when 100 events stream, then scroll and selection remain fixed and an unread counter appears. | `app::tests::pageup_pagedown_adjust_chat_scroll` + `panel::snapshots::chat_unread_counter_pins_to_bottom` |
| Given an approved command, when its arguments change, then approval invalidates. | (server-side enforcement — TUI re-renders the new request) |
| Given a non-interruptible remote action, when interrupt is requested, then replacement waits. | (modal `scope = Reject` keeps plan alive) |
| Given two linked drafts, when either client saves, then neither draft is silently overwritten. | `composer::tests::snapshot_and_restore_round_trip` |
| Given a 40-column monochrome terminal, when approval is requested, then action, risk, and choices remain understandable. | `panel::snapshots::header_narrow_uses_narrow_brand` + `ui::theme::Theme::monochrome` |
| Given a server reconnect, when the last event cursor overlaps, then the transcript contains no duplicate delta. | `app::tests::apply_text_chunks_accumulate_into_streaming` |
| Given 300 sessions updating, when selection is on one row, then background sorting does not move selection. | (placeholder roster — server-driven scrolling TBD) |

## 6. QA Scenario Coverage Matrix

| Scenario | Test | Status |
|---|---|---|
| Header immersive two-line | `header_immersive_two_line_layout` | ✓ |
| Header compact hides row 2 | `header_compact_drops_two_line_row` | ✓ |
| Header narrow mark | `header_narrow_uses_narrow_brand` | ✓ |
| Chat user marker | `chat_renders_user_turn_marker` | ✓ |
| Chat assistant marker | `chat_renders_agent_turn_marker` | ✓ |
| Chat tool success icon | `chat_renders_tool_with_state_icon` | ✓ |
| Chat tool pending icon | `chat_renders_pending_tool_with_running_icon` | ✓ |
| Chat plan inline | `chat_renders_plan_when_present` | ✓ |
| Chat unread counter | `chat_unread_counter_pins_to_bottom` | ✓ |
| Chat CJK content | `chat_handles_cjk_content_without_panic` | ✓ |
| Composer placeholder | `input_renders_placeholder_when_empty` | ✓ |
| Composer multiline | `input_renders_multiline_draft` | ✓ |
| Help approval mode | `help_bar_approval_mode_lists_choices` | ✓ |
| Help normal mode | `help_bar_normal_mode_lists_global_bindings` | ✓ |
| Disconnect offline marker | `disconnected_state_marks_header_offline` | ✓ |
| Enter submits chat | `enter_submits_chat_returns_send_action` | ✓ |
| Shift+Enter newline | `shift_enter_inserts_newline_not_sends` | ✓ |
| IME Enter commits | `ime_enter_commits_candidate_does_not_send` | ✓ |
| Esc quits (no modal) | `esc_quits_when_no_modal` | ✓ |
| Esc dismisses modal | `esc_dismisses_modal_first` | ✓ |
| Approval y sends action | `approval_y_sends_approve_action` | ✓ |
| Ctrl+P opens switcher | `ctrl_p_opens_session_switcher_modal` | ✓ |
| Ctrl+T opens tasks | `ctrl_t_opens_task_overlay_modal` | ✓ |
| `?` opens help | `question_mark_opens_help_overlay` | ✓ |
| PageUp/PageDown scroll | `pageup_pagedown_adjust_chat_scroll` | ✓ |
| Draft snapshot/restore | `composer_snapshot_survives_session_switch` | ✓ |
| CJK width measurement | `ui::cell::tests::*` | ✓ |
| ASCII fallback rendering | `ui::theme::Theme::cached` | ✓ |
| Width breakpoints | `ui::breakpoint::tests::*` | ✓ |
| Middle elision | `ui::cell::tests::middle_elide_keeps_head_and_tail` | ✓ |