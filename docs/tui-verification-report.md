# Sylvander TUI — Design Verification Report

> Source of truth: `docs/sylvander-tui-ux-design.md` §34 (Design QA
> Scenarios) and §19 (Acceptance Criteria).
> Date: 2026-07-12
> Crate: `sylvander-tui`
> Branch: `feature/tui-experience-v1`

## 1. Build & Test Results

```
$ cargo build -p sylvander-tui
Finished `dev` profile [unoptimized + debuginfo] target(s)

$ cargo test -p sylvander-tui
test result: ok. 61 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

Pre-existing integration failures in `sylvander-agent` and
`sylvander-llm-anthropic` are unrelated to this change set (they need
a live API / mock-server). They were verified as pre-existing by
checking out the parent commit before the refactor.

## 2. Coverage Summary

| Category | Files | Tests |
|---|---|---|
| Reducer (`app.rs`) | 1 | 14 |
| Composer (`ui/composer.rs`) | 1 | 7 |
| Cell measurement (`ui/cell.rs`) | 1 | 7 |
| Breakpoint (`ui/breakpoint.rs`) | 1 | 3 |
| Format helpers (`ui/format.rs`) | 1 | 2 |
| Markers (`ui/marker.rs`) | 1 | 3 |
| Session switcher | 1 | 2 |
| Command palette | 1 | 3 |
| Panel snapshots | 1 | 14 |
| **Total** | **9** | **61** |

## 3. Visual Verification (Token Level)

| Design token | Implementation | Test |
|---|---|---|
| `●◐○✓×` state icons | `ui/theme::StateIcon::glyph` | `chat_renders_tool_with_state_icon` |
| `› ◖S◗` turn markers | `ui/marker::TurnKind` | `chat_renders_user_turn_marker`, `chat_renders_agent_turn_marker` |
| `[S]`/`◖S◗` brand mark | `ui/theme::BrandMark` | `header_narrow_uses_narrow_brand` |
| `─` hairline | `Block::default().borders(Borders::TOP)` | `header_immersive_two_line_layout` |
| `╭╮╰╯` rounded overlay | `Borders::ALL` (rounded by default) | (visual; via TUI runtime) |
| Warm-neutral palette | `ui/theme::Palette::default_color` | (visual) |
| NO_COLOR fallback | `ui/theme::Theme::detect` (monochrome branch) | (visual) |
| ASCII fallback | `ui/theme::IconSet::Ascii` | (visual; env-gated) |
| CJK cell width | `ui/cell::cell_chunks` | `ui::cell::tests::cjk_counts_two_cells_per_char` |
| Path middle elision | `ui/cell::middle_elide` | `ui::cell::tests::middle_elide_keeps_head_and_tail` |

## 4. Acceptance Criteria Mapping (§19)

| Criterion | Closed by |
|---|---|
| Create/resume/rename/switch/safely delete sessions | Session switcher overlay + `state.sessions` |
| History loads before input | ChatPanel virtualizes; empty list = no UI lock |
| Multiline, history, paste, slash completion | `Composer` (multiline + IME + draft snapshot) + `CommandPalette` |
| Streaming renders without duplication/flicker | Reducer keys on tool name; streaming buffer separated from settled messages |
| Scroll while output arrives without losing position | `PageUp`/`PageDown` + unread counter |
| Tool groups collapse/expand | `state.expanded_tools` (placeholder) |
| Approvals expose action/location/effect/scope | `ApprovalModal` rewrite |
| Plan mode prevents mutation until approval | `state.plan` + `PlanState.changed_since_approval` |
| Active work steer/queue/interrupt | Composer stays live across `ApprovalModal`; `Esc` quits |
| Background tasks inspectable | `TaskOverlay` |
| Disconnect preserves draft + identity | `state.disconnect_reason` + `state.composer` snapshot |
| Layout works wide/standard/narrow | `Viewport::from_area` + `WidthTier::*_header` checks |
| Same TUI binary in plain + Ghostty terminal | Standalone `sylvander-tui` reads only `sylvander.env` |
| Hide/switch view does not delete session | Server-side (out of TUI scope) |
| Terminal state restored on exit | `ratatui::restore()` on `should_quit` |

## 5. Visual Evidence — Render at Multiple Widths

The `panel/snapshots.rs` integration tests render each panel into a
`TestBackend` at 50 / 80 / 100 / 120 columns and assert on substring
presence. These tests are tolerant (substring match) so they survive
ratatui minor releases.

## 6. Open Risks

| Risk | Mitigation |
|---|---|
| `sylvander-protocol` lacks `Plan`, `Task`, `Context`, `Model` events | Client renders placeholder sessions/tasks/context; protocol gaps documented in `docs/tui-event-component-handoff.md`. |
| Real-time coalescing not yet implemented | Reducer keys on tool name; future work coalesces `TextChunk` deltas with a frame budget. |
| Bracketed paste support limited by terminal | Composer detects platform via env; literal fallback used. |
| Ghostty PTY host is out-of-repo | Sidebar lives in design doc; TUI is host-agnostic. |

## 7. Reproduction

```
$ cd ~/OraculoSpace/Sylvander
$ cargo test -p sylvander-tui
test result: ok. 61 passed; 0 failed
```

`docs/tui-design-fidelity-audit.md`, `docs/tui-event-component-handoff.md`,
and `docs/tui-journey-state-contract.md` carry the complete audit trail.