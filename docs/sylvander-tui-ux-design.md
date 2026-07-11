# Sylvander TUI Experience Design

> Status: Design baseline
>
> Version: 4.1
>
> Date: 2026-07-11
>
> Scope: Terminal-native agent interface and its use inside the Sylvander Ghostty desktop shell

Editable design artifacts:

- [`design/01-experience-map.svg`](./design/01-experience-map.svg) — product-level experience and surface map.
- [`design/02-tui-immersive.svg`](./design/02-tui-immersive.svg) — canonical immersive TUI.
- [`design/03-interaction-states.svg`](./design/03-interaction-states.svg) — plan, approval, AskUser, diff, command, paste, and history states.
- [`design/04-ghostty-sidebar.svg`](./design/04-ghostty-sidebar.svg) — Ghostty desktop with left session sidebar.
- [`design/05-responsive-recovery.svg`](./design/05-responsive-recovery.svg) — narrow, empty, disconnected, and recovery states.
- [`design/06-component-spec.svg`](./design/06-component-spec.svg) — component anatomy and visual states.
- [`design/07-session-management.svg`](./design/07-session-management.svg) — large-scale sessions, notifications, multi-window, and view state.
- [`design/08-execution-control.svg`](./design/08-execution-control.svg) — steer, queue, interrupt, and non-interruptible execution.
- [`design/09-permission-center.svg`](./design/09-permission-center.svg) — pending decisions, scoped rules, history, and revocation.
- [`design/10-transcript-navigation.svg`](./design/10-transcript-navigation.svg) — search, filters, checkpoints, forks, context, and compaction.
- [`design/11-composer-ime.svg`](./design/11-composer-ime.svg) — Chinese IME, attachments, mentions, templates, and draft recovery.
- [`design/12-resilience-operations.svg`](./design/12-resilience-operations.svg) — crashes, reconnect, diagnostics, performance, security, and updates.
- [`design/sylvander-design-tokens.json`](./design/sylvander-design-tokens.json) — color, spacing, typography, and state tokens.
- [`design/README.md`](./design/README.md) — import, editing, and handoff guidance.

## 1. Decision

Sylvander has one canonical conversation interface: `sylvander-tui`.

The same TUI runs in two environments:

1. Directly in any compatible terminal.
2. In a PTY hosted by the Sylvander Ghostty desktop workspace.

Ghostty manages the desktop window, left session sidebar, PTYs, and process lifecycle. It does not implement a second conversation renderer and does not contain agent business logic. `sylvander-server` owns agents, tools, approvals, session history, and persistence.

```text
Normal terminal                  Sylvander Desktop
┌─────────────────────┐          ┌──────────────────────────────┐
│ sylvander-tui       │          │ sessions │ Ghostty PTY/TUI   │
│ one active session  │          │ A active │ active session    │
└──────────┬──────────┘          │ B work   │ immersive output  │
           │                     │ C done   │                    │
           │                     └──────────────┬───────────────┘
           └──────────────┬───────────────────┘
                          ▼
                 sylvander-server
            agent runtime + session store
```

## 2. Design Goals

The visual foundation is Claude Code's calm, conversation-first presentation. Sylvander extends that foundation with stronger execution visibility, session management, task orchestration, multiline input, and progressive tool details.

The interface must:

- Keep the conversation visually dominant.
- Keep the composer stable and usable while the agent works.
- Show verifiable execution progress without flooding the transcript.
- Present routine tool activity compactly and reveal detail on demand.
- Make plans, approvals, interruptions, and background tasks explicit.
- Support persistent sessions without requiring a permanent sidebar.
- Render consistently in Ghostty and other compatible terminals.
- Remain fully keyboard accessible; mouse support is supplementary.
- Adapt cleanly from wide desktop terminals to narrow terminal windows.

### 2.1 Product character

Sylvander should feel calm, capable, and quietly alive. It is not a dashboard full of chrome and it is not a raw log viewer. The conversation is spacious; execution is precise; risk is explicit; motion is restrained. Crab energy appears through identity, language, and occasional microcopy rather than decorative noise.

The desired aesthetic is **quiet technical confidence**:

- Dark, warm-neutral canvas instead of pure black.
- Soft ivory primary text instead of harsh white.
- Coral accent for identity and selection, used sparingly.
- Teal for verified success and blue for active work.
- Conversation and tool output sit directly on the canvas without gray containers.
- Borders are reserved for input focus, decisions, menus, and inspectable overlays.
- Strong alignment, indentation, and whitespace replace heavy separators and cards.
- Symbols always paired with text so meaning survives monochrome terminals.

### 2.2 Brand mark and session entry

Sylvander uses a compact mark rather than a large ASCII-art banner. The goal is the clarity of Codex/Qwen entry screens without copying their geometry or consuming the conversation viewport.

The mark is a minimal crab-shell symbol: two open claws around a central `S`. It expresses crab energy at small sizes and remains recognizable in monochrome.

```text
  ◖S◗  SYLVANDER
       intelligent terminal workspace

       ~/Projects/acme-api
       What are we building today?
```

| Context | Mark | Rule |
|---|---|---|
| Ghostty/native vector | Two coral claw arcs + central shell | May use vector curves; no enclosing badge |
| Unicode terminal | `◖S◗ SYLVANDER` | Primary terminal wordmark |
| ASCII fallback | `[S] SYLVANDER` | Used when glyph width is uncertain |
| Active session header | `◖S◗` or `Sylvander` | Never show the full welcome lockup inside a conversation |
| Narrow terminal | `S ›` | One-cell identity plus prompt direction |

Rules:

- Maximum welcome lockup height is five terminal rows.
- Brand appears once on entry, not before every assistant response.
- No gradient-filled badge, giant block letters, mascot illustration, or animated logo loop.
- Coral is limited to the mark; `SYLVANDER` uses primary text color.
- Version, model, workspace, and permission mode appear as quiet metadata below the welcome prompt, not inside the logo.

### 2.3 Design synthesis from leading agents

Sylvander does not copy one product wholesale. It combines proven strengths and deliberately rejects their weaker patterns.

| Source | Strength adopted | Sylvander interpretation | Pattern not adopted |
|---|---|---|---|
| Claude Code | Calm conversation flow, concise tool narration, plan-first collaboration | Transcript remains primary; agent explains intent before tool groups | Tool activity becoming difficult to revisit after long runs |
| Codex CLI | Clear working state, approvals, plan tracking, steering while running | Explicit interruption and evidence-oriented completion | Gray boxed output and excess status chrome competing with immersion |
| Gemini CLI | Discoverable commands and rich interactive controls | Contextual slash palette and mode-aware help | Dense permanent footer instructions |
| Kimi Code | Session continuity, compact long-running-agent behavior | Persistent sessions, resumable drafts, background task summaries | Hidden state that requires reading raw logs |
| Qwen Code | Subagent and team visibility | Task overlay showing owner, progress, and latest activity | Treating orchestration as a separate primary workspace |
| OpenCode | Strong navigation and detailed permission surfaces | Searchable sessions and inspectable decisions | Permanent application panels at ordinary terminal widths |
| Pi / minimal agents | Low visual noise and direct interaction | Default screen remains simple until complexity is needed | Minimalism that hides safety, context, or execution evidence |

The resulting hierarchy is:

1. Conversation.
2. Current intent and execution.
3. Composer.
4. Session and environment context.
5. On-demand navigation, tools, tasks, and diagnostics.

### 2.4 Experience principles

1. **Conversation before machinery.** Tool internals appear only when they help understanding, trust, or recovery.
2. **Stable surfaces.** Header, transcript, composer, and footer do not jump as content streams.
3. **Progressive disclosure.** Borderless one-line summaries expand into aligned full inputs, diffs, output, timing, and provenance.
4. **Evidence at completion.** “Done” includes tests, changed files, or other concrete verification when applicable.
5. **Risk near the decision.** Approval cards place action, effect, scope, and working directory together.
6. **Keyboard fluency without memorization.** Common actions are fast; contextual hints teach them in place.
7. **Session continuity.** A sidebar item is a view onto durable work. Hiding or closing a view never silently destroys its session.
8. **Graceful degradation.** Narrow terminals, low color, missing Unicode, and lost connections remain usable.

## 3. Information Architecture

Sylvander uses four persistent regions and three temporary layers.

```text
  Header: identity · session · environment

  Transcript viewport
  conversation · plans · tools · decisions · errors
  no surrounding panel and no per-turn gray card

  ─ Composer: draft · attachments · steering ─
  Status: mode · activity · context · permissions · connection
  Contextual key hints

Temporary layers:
  palette/switcher     approval/ask decision     inspect/detail drawer
```

### 3.1 Primary objects

| Object | User meaning | Lifetime | Primary presentation |
|---|---|---|---|
| Workspace | Files and execution boundary | Durable | Header and session metadata |
| Session | One resumable body of work | Durable | Transcript and session switcher |
| Turn | One user instruction and agent response cycle | Durable | Transcript grouping |
| Plan | Agreed sequence of work | Session or turn | Inline progress region |
| Tool operation | Read, edit, command, search, MCP, or external action | Turn | Collapsible execution row |
| Decision | Permission or answer required from user | Until resolved | Focused decision card |
| Task | Main-agent, subagent, or background activity | Until resolved | Compact summary and task overlay |
| Draft | Unsent user input | Until sent or discarded | Composer, persisted per session |

### 3.2 Navigation model

- The ordinary state has no sidebar.
- `Ctrl+P` opens global session navigation.
- `Ctrl+T` opens tasks and subagents.
- `/` opens commands scoped to the current state.
- `Ctrl+O` inspects the selected transcript object.
- `Esc` always moves one layer back before it can interrupt or exit.
- Ghostty's left sidebar provides desktop-level parallel visibility; the TUI session switcher provides search and resume in ordinary terminals.

## 4. Evidence from the Current Repository

| Area | Status | Current evidence | Design consequence |
|---|---|---|---|
| TUI framework | Confirmed | `sylvander-tui` uses Ratatui and Crossterm | Retain terminal-native rendering |
| State model | Confirmed | `sylvander-tui/src/app.rs` has a reducer-style `AppState` | Extend the state model instead of replacing it |
| Component model | Confirmed | Panels implement `Component`; modals use a stack | Preserve layered rendering, add focus and overlay contracts |
| Streaming | Confirmed | Text and thinking deltas already accumulate independently | Render stable live blocks without transcript duplication |
| Tool approvals | Confirmed | Approval events and modal handling already exist | Redesign the presentation as an inline contextual decision card |
| Input | Confirmed | `InputState` is currently single-line | Replace with multiline editing and history |
| Scrolling | Confirmed | `chat_scroll` exists, but normal key handling is not wired | Add viewport navigation and bottom-follow behavior |
| Session transport | Confirmed | TUI mirrors Unix-channel wire types locally | Move shared commands and events into `sylvander-protocol` |
| Server startup | Confirmed | The server currently starts HTTP and optional DingTalk channels | Wire the local TUI transport before desktop integration |
| Ghostty boundary | Confirmed | Project guidance describes Ghostty as substrate without business logic | Host the TUI through PTYs rather than build a Swift chat UI |
| Persistent storage | Pending implementation | Server creates an in-memory SQLite session store | Add durable storage and resume semantics |

## 5. Canonical Conversation Screen

The reference viewport is 120 columns by 36 rows.

```text
  Sylvander 🦀  auth-refactor                     claude-sonnet-5 · plan
  ~/Projects/acme-api · feat/auth-refactor · session 8f21
  ───────────────────────────────────────────────────────────────────────────

  You
  Add JWT authentication. First inspect the existing middleware and propose
  a plan before changing anything.

  Oraculo
  I’ll inspect the authentication flow and identify where token validation
  belongs.

  ● Exploring the codebase                                            12s
     ✓ Read     src/http/router.rs                              126 lines
     ✓ Search   "middleware" in src/                           14 matches
     ✓ Read     src/auth/mod.rs                                 82 lines
     ◐ Inspect  tests/auth_test.rs                              running…

  The project already has session-cookie authentication in `src/auth/mod.rs`.
  JWT support can share its identity extraction layer.

  Proposed plan

  1. Extract `AuthenticatedUser` from the existing cookie middleware.
  2. Add a JWT verifier implementing the same authentication interface.
  3. Register both mechanisms in the HTTP authentication layer.
  4. Add unit and integration coverage.

  No files have been changed yet.

  ───────────────────────────────────────────────────────────────────────────
  Ask Sylvander…
  │
  ───────────────────────────────────────────────────────────────────────────
  normal · plan mode   context 24%   3 tools   main
  ↵ send   ⇧↵ newline   esc interrupt   ctrl+r history   / commands   ? help
```

### 5.1 Header

The header is a quiet two-line text region plus one hairline separator. It has no surrounding panel. It shows:

- Session name.
- Workspace path.
- Git branch when available.
- Selected model.
- Short session identifier.

Secondary metadata collapses before it wraps. The header must never displace the composer.

### 5.2 Transcript

The transcript is semantic rather than a raw event log. It sits directly on the terminal canvas, with no outer container and no gray box around turns. It contains user messages, assistant messages, execution groups, plans, errors, and decisions.

Rules:

- Assistant streaming updates one live block in place.
- Completed streaming content becomes one transcript entry.
- Thinking is collapsed by default and must be visually distinct.
- Tool events belonging to one agent step are grouped.
- Grouping uses indentation, alignment, symbols, and whitespace—not filled cards.
- A faint vertical guide may connect active child operations; it disappears after completion unless focused.
- Successful low-information operations remain one line.
- Failures remain visible until acknowledged or superseded.
- The viewport follows the bottom unless the user scrolls upward.
- New output must not steal the viewport while the user reads history.

### 5.3 Composer

The composer is multiline and remains anchored above the status rows. Its resting state uses whitespace and horizontal rules; a subtle accent outline appears only while focused, receiving a paste, or requesting a steering decision.

Required behavior:

- `Enter` sends.
- `Shift+Enter` inserts a newline.
- Arrow keys edit text when the composer owns focus.
- `Ctrl+R` searches prompt history.
- `/` opens the command palette when entered at the start of an empty prompt.
- Pasted multiline content is preserved and visually bounded.
- Large pastes show a compact attachment-like summary with an expand action.
- Draft text survives modal interactions and session switching.

## 6. Active Execution

Tool work is grouped into one live execution rhythm without a surrounding container.

```text
  Oraculo
  I found two authentication paths. I’m checking which one is active in
  production before editing.

  ● Working                                                        00:18
    ├─ ✓ Search auth configuration                         14 matches
    ├─ ✓ Read src/config/auth.rs                            82 lines
    ├─ ◐ Run cargo test auth                                running…
    └─ ○ Inspect deployment configuration                   queued

  ───────────────────────────────────────────────────────────────────────────

  You can type while I work. Your message will steer the current turn…
  │
  ───────────────────────────────────────────────────────────────────────────
  working · esc interrupt   shift+tab switch mode   ctrl+t tasks
```

The composer stays usable during execution. A submitted message can:

- Steer the current turn.
- Queue the next instruction.
- Interrupt and replace the current instruction.

When intent is ambiguous and the distinction matters, Sylvander presents those three choices rather than guessing.

## 7. Tool Presentation

Routine results are compact:

```text
  ✓ Read src/auth/mod.rs                                  126 lines
  ✓ Edited src/auth/jwt.rs                              +48  -3
  ✓ cargo test auth                                      18 passed
```

Selected items expand in place. Diff detail uses syntax color and aligned gutters, not a gray card:

```text
  ▾ Edited src/auth/jwt.rs                              +48  -3
  41 │ + pub struct JwtVerifier {
  42 │ +     decoding_key: DecodingKey,
  43 │ +     validation: Validation,
     │
  67 │ - validate_session_cookie(cookie)
  67 │ + authenticate_request(request).await
```

Long command output is collapsed by default:

```text
  ✓ cargo test --workspace
    138 passed · 0 failed · completed in 8.4s
    [enter to expand output]
```

Tool display order is stable: state icon, action, target, result summary, duration. Color reinforces state but never replaces the icon or label.

## 8. Approval Request

Approvals appear as contextual decision cards over the transcript.

```text
╭─ Permission required ───────────────────────────────────────────────────────╮
│                                                                            │
│  Run command                                                               │
│                                                                            │
│    cargo test --workspace                                                  │
│                                                                            │
│  Working directory                                                         │
│    ~/Projects/acme-api                                                     │
│                                                                            │
│  This command runs project tests and writes only build artifacts.          │
│                                                                            │
│  › 1. Allow once                                                           │
│    2. Allow commands beginning with `cargo test`                            │
│    3. Reject and tell Sylvander what to do                                  │
│                                                                            │
╰────────────────────────────────────────────────────────────────────────────╯
  ↑↓ choose   enter confirm   esc reject
```

Every approval shows the exact action, working directory, effect summary, and approval scope. Rejection may include feedback. The permanent rule option must describe its precise matching scope.

## 9. Plan Mode

Plan mode is an interaction contract, not only a status label.

```text
  Oraculo
  Here is the implementation plan.

  ┌─ Plan ──────────────────────────────────────────────────────────────────┐
  │ ✓ 1. Inspect the current authentication boundary                       │
  │ ● 2. Define the JWT verification interface                             │
  │ ○ 3. Implement verifier and middleware                                  │
  │ ○ 4. Add unit and integration tests                                     │
  │ ○ 5. Run workspace verification                                         │
  └─────────────────────────────────────────────────────────────────────────┘

  Waiting for approval before editing files.

╭─────────────────────────────────────────────────────────────────────────────╮
│ Approve the plan or describe what you want changed…                        │
╰─────────────────────────────────────────────────────────────────────────────╯
  plan · no files changed   enter approve   e edit plan   esc cancel
```

The plan updates in place during implementation. Completed, active, pending, blocked, and skipped steps have distinct text labels and icons. Plan mode does not mutate project files until the user approves implementation.

## 10. Sessions

`Ctrl+P` or `/sessions` opens a temporary overlay. There is no permanent sidebar in the standalone TUI.

```text
╭─ Sessions ──────────────────────────────────────────────────────────────────╮
│ Search sessions… auth_                                                      │
├─────────────────────────────────────────────────────────────────────────────┤
│ ● auth-refactor       working     ~/Projects/acme-api              2m ago   │
│   auth-debug          waiting     ~/Projects/acme-api              1h ago   │
│   login-tests         complete    ~/Projects/web                   yesterday│
│   jwt-research        complete    ~/Notes                          Jul 9     │
├─────────────────────────────────────────────────────────────────────────────┤
│ enter open   ctrl+n new   ctrl+w close view   r rename   d delete session  │
╰─────────────────────────────────────────────────────────────────────────────╯
```

In a normal terminal, opening a session replaces the current view. In the Sylvander Ghostty desktop, selection activates the session in the main PTY region and the previous session continues independently. Hiding a sidebar item does not delete the persisted session.

Session state values are `working`, `waiting`, `complete`, `failed`, and `disconnected`. Destructive deletion always requires confirmation.

## 11. Tasks and Subagents

Background activity is summarized in the transcript:

```text
  ● 3 tasks running

    main       Implementing JWT middleware                         working
    explorer   Comparing existing authentication paths             72%
    tester     Running authentication integration tests            11/18

  [ctrl+t to inspect tasks]
```

`Ctrl+T` opens the task overlay:

```text
╭─ Tasks ─────────────────────────────────────────────────────────────────────╮
│                                                                            │
│ ● main       Implement JWT middleware                             00:42     │
│   └─ editing src/auth/jwt.rs                                                │
│                                                                            │
│ ● explorer   Compare authentication paths                         00:31     │
│   └─ 8 files inspected · report ready                                      │
│                                                                            │
│ ● tester     Authentication integration tests                     00:18     │
│   └─ 11 passed · test_refresh_token running                                 │
│                                                                            │
╰─────────────────────────────────────────────────────────────────────────────╯
```

Each task exposes owner, purpose, state, elapsed time, latest activity, and available actions such as inspect, steer, interrupt, or dismiss.

## 12. Command Palette

Typing `/` opens contextual completion:

```text
╭─ Commands ──────────────────────────────────────────────────────────────────╮
│ /mo                                                                         │
├─────────────────────────────────────────────────────────────────────────────┤
│ /model        Change model                                                  │
│ /mode         Switch plan / normal / autonomous mode                        │
│ /memory       View or update project memory                                 │
├─────────────────────────────────────────────────────────────────────────────┤
│ ↑↓ select   enter run   esc close                                           │
╰─────────────────────────────────────────────────────────────────────────────╯
```

Initial command set:

```text
/new            /sessions       /resume
/model          /mode           /agents
/tasks          /diff           /review
/context        /compact        /memory
/permissions    /tools          /mcp
/clear          /help           /quit
```

Commands may be unavailable based on capability or state. Unavailable commands remain discoverable and explain why they cannot run.

### 12.1 AskUser variants

AskUser uses the same focused decision layer as approval but has three content modes:

- **Single select:** arrow keys choose, number keys jump, Enter confirms.
- **Multi select:** Space toggles choices, Enter confirms the set.
- **Free text:** Composer behavior is reused, including multiline input and paste handling.

The original question remains visible after answering as a compact transcript decision with the selected answer. Deferring a question is distinct from rejecting the agent's work.

### 12.2 Plan review and editing

Plan review begins as an immersive inline region. The selected step receives a faint coral wash; the plan does not gain a surrounding gray panel.

- Enter approves the plan.
- `e` edits the selected step inline.
- `a` adds a step after the selection.
- `d` removes a step with undo.
- Drag/mouse or `Alt+↑/↓` reorders steps.
- A changed plan shows who changed it and waits for renewed approval if scope materially changed.

### 12.3 Diff and tool inspection

Expanded diffs retain the transcript background. Line numbers form a muted gutter; additions and removals use text color without full-width green/red backgrounds. Large diffs open in an inspect layer with file and hunk navigation, but collapsing returns to the same scroll position.

Command output follows the same rule: compact summary first, borderless aligned lines second, dedicated inspect layer only for long or interactive output.

### 12.4 Paste, attachments, and prompt history

- Pasted content under eight lines stays inline.
- Larger pastes become a one-line object: kind, line count, byte size, and preview action.
- File/image references appear as removable tokens above the draft, never inside the transcript before sending.
- `Ctrl+R` searches per-session prompt history; a second shortcut expands search across sessions.
- Restoring a historical prompt never overwrites the current draft without keeping an undo snapshot.

### 12.5 Session/workspace launch flow

```text
New session
  → choose recent workspace / folder / no workspace
  → choose new session or resume matching session
  → create sidebar item (Ghostty) or replace view (standalone)
  → restore history and draft
  → focus composer only after restoration completes
```

Failures remain in the launcher with an actionable explanation. The user never lands in an empty conversation that silently lost its intended workspace.

## 13. Responsive Layout

Below approximately 80 columns, secondary metadata collapses and labels shorten:

```text
┌─ Sylvander 🦀 ─ auth-refactor ───────────┐
│ ~/acme-api · feat/auth-refactor          │
└──────────────────────────────────────────┘

  You
  Add JWT authentication.

  Oraculo
  I’ll inspect the existing middleware.

  ● Exploring
    ✓ Read src/http/router.rs
    ◐ Search authentication code

╭──────────────────────────────────────────╮
│ Ask Sylvander…                           │
╰──────────────────────────────────────────╯
  working · ctx 24% · esc interrupt
```

Responsive rules:

- Wide: 100 columns and above; full metadata and descriptions.
- Standard: 80–99 columns; compact metadata and tool summaries.
- Narrow: below 80 columns; single-column semantic regions and minimal status.
- Minimum supported viewport: 50 columns by 12 rows.
- Below the minimum, show a clear resize message without corrupting terminal state.

## 14. Focus and Navigation

The TUI has explicit focus targets: transcript, composer, overlays, and expanded tool output.

Default bindings:

| Key | Action |
|---|---|
| `Enter` | Send or activate selected item |
| `Shift+Enter` | Insert composer newline |
| `Esc` | Close overlay, interrupt active work, or clear focus; never quit immediately |
| `Ctrl+C` | First press interrupts active work; second press exits when idle |
| `Ctrl+P` | Open session switcher |
| `Ctrl+T` | Open tasks |
| `Ctrl+R` | Search prompt history |
| `Shift+Tab` | Cycle agent mode |
| `PageUp` / `PageDown` | Scroll transcript |
| `Ctrl+O` | Expand or collapse selected tool details |
| `/` | Open command completion at prompt start |
| `?` | Open contextual help |

Bindings must be configurable later, but the initial implementation should keep one documented default map.

## 15. Visual Language

State icons:

| Icon | Meaning |
|---|---|
| `●` | Active or selected |
| `◐` | Running |
| `○` | Queued or pending |
| `✓` | Successful |
| `!` | Warning or attention required |
| `×` | Failed or rejected |

Color policy:

- Default terminal foreground for conversation text.
- Muted gray for metadata and completed secondary information.
- Accent color for selection and active state.
- Green for verified success.
- Yellow for waiting, warning, or approval.
- Red for failure and destructive actions.

All meaning must remain understandable with color disabled. Respect `NO_COLOR` and terminal capability detection.

## 16. Ghostty Desktop Experience

Each visible Ghostty session view owns a PTY child process running `sylvander-tui` with an explicit session identity and workspace. Views are selected from a persistent left sidebar rather than a top tab strip.

The desktop shell is intentionally thin but visually coherent with the TUI:

```text
┌────────────────────────────────── Sylvander ───────────────────────────────┐
│ SESSIONS              │  Sylvander 🦀  api-review           sonnet · plan │
│                       │  ~/Projects/acme-api · feat/api-review             │
│ ＋ New session         │  ────────────────────────────────────────────────  │
│                       │                                                    │
│ ◐ auth-refactor       │  You                                               │
│ ● api-review          │  Review the public API before release.             │
│ ✓ release-notes       │                                                    │
│   jwt-research        │  Oraculo                                           │
│                       │  I found two compatibility risks.                   │
│ WORKSPACES            │                                                    │
│ ▾ acme-api            │  ● Reviewing public surface                 00:31  │
│   ▸ web               │     ✓ Compare exported types                       │
│                       │     ◐ Trace deprecated method callers              │
│                       │                                                    │
│                       │  ────────────────────────────────────────────────  │
│ ⚙ Settings            │  Ask Sylvander…                                    │
│                       │  ────────────────────────────────────────────────  │
│                       │  working · context 31% · 2 tools                    │
└───────────────────────┴────────────────────────────────────────────────────┘
```

### 16.1 Sidebar semantics

- The sidebar is 24–32 terminal columns wide and can collapse to a 3-column state rail.
- Sessions are grouped by workspace, with a flat recent list above optional workspace groups.
- One selected sidebar item displays one session in the main PTY region.
- Session title uses the user-defined name, falling back to a concise generated task name.
- Leading state indicator: `◐` working, `●` waiting for user, `✓` completed, `!` failed, no icon when idle.
- Working indicators update without moving neighboring rows.
- Unsaved composer drafts receive a subtle dot independent of agent activity.
- Hiding a view or switching sessions never deletes or interrupts the session.
- Sidebar search replaces the list in place; it does not cover the active transcript.
- Reopening the desktop restores sidebar expansion, ordering, selected session, scroll position, and drafts.
- Focus mode collapses the sidebar; moving the pointer to the left edge or pressing `Cmd+Shift+S` reveals it temporarily.

### 16.2 Desktop-level actions

| Action | Default | Result |
|---|---|---|
| New session | `Cmd+T` | Opens workspace/session launcher and adds a sidebar item |
| Toggle sidebar | `Cmd+Shift+S` | Expands, collapses, or temporarily reveals the session sidebar |
| Search sessions | `Cmd+K` | Focuses sidebar search; selection changes the active session view |
| Next/previous session | `Ctrl+Tab` / `Ctrl+Shift+Tab` | Moves through sidebar recency without affecting execution |
| Hide view | `Cmd+W` | Removes the item from the visible recent list; session remains durable |
| Reopen hidden view | `Shift+Cmd+T` | Restores the previous session and its draft |

### 16.3 Workspace/session launcher

New-session flow is fast for repeat work and explanatory for first-time use:

```text
╭─ New Sylvander session ────────────────────────────────────────────────────╮
│                                                                           │
│  Recent workspaces                                                        │
│  › ~/Projects/acme-api                         last used 8 minutes ago     │
│    ~/Projects/sylvander                         last used yesterday         │
│                                                                           │
│  [ Choose folder… ]    [ Resume session… ]    [ Start without workspace ] │
│                                                                           │
╰───────────────────────────────────────────────────────────────────────────╯
```

After workspace selection, the session appears in the sidebar immediately and the TUI owns the rest of the interaction. The native shell does not add a second composer, transcript, permission sheet, or agent status model.

### 16.4 Native/TUI visual relationship

- Ghostty chrome uses the same warm-neutral background family as the TUI but is one luminance step darker.
- Active sidebar item uses a slim coral leading rule and brighter text, not a filled gray rectangle.
- Native controls disappear in fullscreen/focus mode; the TUI remains fully operable.
- Window vibrancy, if enabled, is restricted to the sidebar and title region so transcript contrast remains stable.
- The terminal grid determines all conversation spacing; the shell never overlays content inside it.

Conceptual launch forms:

```text
sylvander-tui --new --workspace <path>
sylvander-tui --session <session-id>
```

The native shell may:

- Show, hide, reorder, and search session views in the sidebar.
- Restore sidebar items by persisted session identifier.
- Reflect session state in sidebar indicators.
- Open a selected session in the main PTY region.
- Request a graceful TUI shutdown before terminating the PTY.

The native shell must not:

- Render assistant messages or tool calls itself.
- Implement approvals or agent modes.
- Own conversation history.
- Duplicate session or protocol state already owned by the server.

## 17. Empty, Error, and Recovery States

### 17.1 First launch

The first launch avoids a blank chat box. It uses the compact Sylvander wordmark, one question, and three low-pressure starting actions:

```text
                         ◖S◗  SYLVANDER
                              intelligent terminal workspace

                 What are we building today?

        [ Open a workspace ]  [ Resume a session ]  [ Ask anything ]

        /help for commands · your work stays local to this session
```

### 17.2 Disconnection

Connection loss never clears the transcript or composer. A non-modal banner shows reconnect progress, while destructive or ambiguous sends are queued visibly:

```text
  ! Server disconnected · reconnecting in 2s · draft preserved
```

### 17.3 Interrupted turn

An interrupted turn remains in history with its partial response and a clear terminal state. The user can continue, retry from the last safe boundary, or inspect completed tool actions.

### 17.4 Failure hierarchy

- Inline row: individual tool failure with a recoverable next step.
- Emphasized transcript region: turn-level failure requiring user attention, without a filled gray card.
- Banner: connection, authentication, or server availability problem.
- Full-screen recovery: terminal capability or state restoration failure only.

## 18. Accessibility and Terminal Compatibility

- All state is communicated through icon, word, and optionally color.
- Focus is always visible and announced by border/title changes.
- Reduced-motion mode replaces spinners with textual state changes.
- `NO_COLOR` is honored.
- ASCII fallback replaces box drawing and Unicode state icons.
- Screen-reader mode linearizes semantic regions and avoids in-place animated rewriting.
- High-contrast themes preserve at least three distinguishable luminance levels.
- Mouse targets never replace keyboard equivalents.
- Copy mode exposes transcript text without decorative prefixes where possible.

## 19. Acceptance Criteria

The baseline design is complete when:

- A user can create, resume, rename, switch, and safely delete sessions.
- Conversation history loads before accepting new input.
- Multiline composition, prompt history, paste handling, and slash completion work.
- Streaming text, thinking, tools, and completion render without duplication or flicker.
- Users can scroll history while new output arrives without losing their position.
- Tool groups collapse and expand without altering transcript order.
- Approvals expose action, location, effect, and scope.
- Plan mode prevents project mutation until implementation is approved.
- Active work can be steered, queued, or interrupted.
- Background tasks and subagents are inspectable.
- Disconnects preserve the draft and session identity and offer reconnect.
- The layout works at wide, standard, and narrow widths.
- The exact same TUI binary works in regular terminals and the Sylvander Ghostty workspace.
- Hiding or switching a Ghostty session view does not delete its session.
- Terminal state is restored after normal exit, panic, disconnect, and interrupt.

## 20. Design-to-Delivery Sequence

1. Stabilize shared protocol and persistent session semantics.
2. Build the transcript viewport and semantic event model.
3. Replace single-line input with the multiline composer.
4. Add stable streaming and grouped tool presentation.
5. Add approvals, plan mode, interruption, and steering.
6. Add session switcher, history restoration, and reconnect.
7. Add commands, tasks, subagent visibility, and context status.
8. Verify responsive behavior and terminal compatibility.
9. Integrate the unchanged TUI binary into the Ghostty session workspace.
10. Add desktop sidebar restoration and session-state indicators.

## 21. Non-Goals for the Baseline

- A separate SwiftUI conversation renderer.
- Agent execution or tool logic inside Ghostty.
- Pixel-identical rendering across terminal fonts.
- Mouse-only interactions.
- A permanently visible session sidebar in the standalone TUI. The Ghostty desktop intentionally has a collapsible left session sidebar.
- Rich GUI widgets that cannot degrade to terminal cells.

## 22. Session System and Sidebar at Scale

### 22.1 Combined session state

A session can have one execution state and independent view flags. The sidebar must not compress these into one ambiguous dot.

| Dimension | Values | Presentation |
|---|---|---|
| Execution | idle, working, waiting, paused, complete, failed, reconnecting | Leading icon + text in expanded sidebar |
| Draft | clean, unsent | Small trailing dot; never replaces execution icon |
| Attention | none, unread, approval, question, failure | Count or semantic badge |
| Visibility | recent, pinned, archived, hidden | Sidebar section and command availability |
| Connection | local, remote, offline | Metadata only unless degraded |

Priority when space is limited: approval/question → failure → working → unread → draft → complete → idle.

### 22.2 Large session collections

The sidebar scales beyond a recent list:

- **Recent:** recency-ordered views, capped visually but searchable.
- **Pinned:** manually ordered durable shortcuts.
- **Waiting for you:** automatically promoted approvals and questions.
- **Workspaces:** collapsible grouping with per-workspace counts.
- **Archived:** excluded from normal navigation but globally searchable.

Search matches title, workspace, branch, message text, files, tags, and session ID. Filters include state, workspace, model, agent, date, and tag. Hundreds of sessions use virtual scrolling; selection remains stable while background states update.

### 22.3 Session switching contract

Switching views never implicitly interrupts work. The system preserves transcript position, live-follow mode, selected object, composer draft, history search, and expanded tool rows per session.

Special cases:

- Streaming continues and the sidebar indicator updates.
- Pending approval is promoted to **Waiting for you** and may notify.
- A session opened by another client shows a linked-view marker, not a lock.
- Conflicting draft edits create two labeled drafts; no last-write-wins loss.
- Deleted or archived remote sessions remain recoverable until the user acknowledges the change.

### 22.4 Notifications

Notifications are generated for waiting approval, AskUser, failure, long-running completion, and explicit agent mention. They are suppressed for the active visible session unless the window is unfocused. Per-workspace quiet hours and “notify only when waiting for me” are supported.

Sidebar state is authoritative; system notifications are transient pointers back to it.

### 22.5 Multi-window behavior

- A session can be moved to another window or opened as a linked view.
- Each window owns sidebar expansion, selection, geometry, and focus mode.
- Session execution, history, permissions, and draft versions remain server-owned.
- Closing the last window does not terminate server-side work unless configured.
- A workspace-focused window may filter its sidebar without changing global session visibility.

## 23. Execution Control: Steer, Queue, Interrupt

### 23.1 Submission behavior while working

When the composer submits during active execution, Sylvander chooses the least disruptive valid action and makes it visible:

| Situation | Default | Reason |
|---|---|---|
| Agent is reasoning or between tools | Steer | New guidance can affect the current turn safely |
| A non-interactive tool is running | Queue | Avoid implying that the external process received guidance |
| User explicitly presses interrupt shortcut | Interrupt | Intent is unambiguous |
| Destructive action is pending approval | Update/reject decision | Never queue behind an unresolved risk decision |
| Agent has emitted final answer | New turn | Current turn is already terminal |

If confidence is low, a compact chooser appears above the composer: **Steer current**, **Queue next**, **Interrupt and run**.

### 23.2 Steer

Steering messages enter the current turn at the next safe agent boundary. The transcript displays them as user interventions with a `steered` label. Multiple unsent steering messages can be edited, reordered, or merged before consumption.

The agent acknowledges the changed intent before further mutation. A steer does not pretend to alter a command already executing.

### 23.3 Queue

Queued prompts appear immediately above the composer as a numbered borderless list. Users can edit, reorder, delete, or promote an item to interrupt. Each item retains attachments and mode overrides.

Queue execution begins only after the current turn reaches done, failed, or interrupted. A waiting approval pauses queue advancement.

### 23.4 Interrupt

Interrupt is a state transition, not an undo operation:

1. Signal the agent loop and cancellable tools.
2. Mark non-cancellable external work as “stopping” or “continues externally.”
3. Preserve partial assistant output and completed tool evidence.
4. Close orphaned tool calls with an explicit interrupted result.
5. Start the replacement instruction only after the interruption boundary is recorded.

The UI never implies filesystem rollback. When changes occurred, it offers **Inspect changes**, **Revert selected**, or **Continue from current state** as separate actions.

### 23.5 Non-interruptible work

For atomic writes, remote deployment, or other unsafe cancellation points, `Esc` changes the status to **interrupt requested**. The UI explains what is still running, why immediate cancellation is unsafe, and the next boundary where control returns.

## 24. Permission Center and Decision Lifecycle

### 24.1 Permission scopes

Every reusable permission is explicit about action, resource, workspace, and lifetime:

| Scope | Example | Lifetime |
|---|---|---|
| Once | Exact command invocation | One decision |
| Turn | Reads under `src/` | Current turn |
| Session | `cargo test` prefix in current workspace | Session lifetime |
| Workspace | Writes under `docs/` | Until revoked |
| Global | Trusted MCP server read-only tools | Until revoked |

Global and workspace rules require a review step and are never the preselected option.

### 24.2 Permission Center structure

```text
Permissions
  Pending decisions
  Session rules
  Workspace rules
  Global rules
  Decision history
```

Each rule shows matcher, effect, origin, creator, created time, last use, and revoke action. Vague patterns such as “all safe commands” are not representable.

### 24.3 Multiple and remote decisions

- Pending decisions are ordered by dependency, then time.
- Independent read-only decisions may be approved as a reviewed batch.
- Destructive decisions remain separate.
- A decision handled by another client updates all views and records the actor.
- Switching sessions does not dismiss a decision; it moves to the sidebar waiting section.
- Timeouts resolve to deny unless a stricter policy specifies otherwise.

### 24.4 Invalidating approval

An approval becomes invalid if command text, arguments, working directory, environment mutation, target resource, tool identity, or risk classification changes. The replacement request highlights the difference from the previously reviewed action.

### 24.5 Audit and security

Decision history records requested action, effective rule, actor, result, execution correlation, and revocation. Secrets are redacted before presentation and storage. Tool provenance is shown as Built-in, Skill, MCP, or external provider.

## 25. Transcript Navigation, Checkpoints, and Context

### 25.1 Search and semantic navigation

Transcript search is a temporary top layer that preserves the underlying scroll position. It supports plain text, exact phrase, and filters:

- Author: user, main agent, named subagent.
- Kind: message, tool, diff, approval, error, checkpoint.
- Resource: file path, command, MCP server, URL.
- Time and turn range.

Keyboard navigation includes next/previous match, next/previous user turn, next failure, next decision, and return to live. When the user is reading history, new events accumulate behind a stable `3 new events · return to live` indicator.

### 25.2 Checkpoints and forks

A checkpoint names a stable conversational and workspace reference. It records session cursor, plan, context summary, changed-file snapshot metadata, and verification state.

Forking from a turn creates a new durable session with explicit ancestry. It does not silently copy or roll back the filesystem. The fork dialog therefore offers:

- Same current workspace state.
- New Git branch from checkpoint commit, when available.
- New worktree.
- Conversation-only fork with no filesystem claim.

Session comparison shows divergent user instructions, agent decisions, file changes, and verification results.

### 25.3 Export and provenance

Sessions export to readable Markdown and lossless JSON. Export options independently include thinking, tool inputs, tool output, diffs, approvals, and environment metadata. Secret redaction is enabled by default and reported in the export summary.

### 25.4 Context and compaction

Context display explains state rather than showing an unexplained percentage:

```text
context 72% · 118k / 164k usable · compaction after this turn
```

Users can inspect the projected context: system instructions, durable summary, recent turns, attachments, and tool artifacts. Automatic compaction creates a visible checkpoint with a summary of what was retained, collapsed, or excluded.

Manual `/compact` previews the intended boundary. A compaction failure preserves the previous context and offers model switch, attachment removal, or a new session fork.

### 25.5 Model and mode switching

The model picker reports provider, context capacity, tool/thinking/image support, latency class, and availability. Switching during a turn is queued unless the user explicitly interrupts. Unsupported capabilities are described before confirmation.

Plan, Normal, and Autonomous modes show a short behavior description and permission impact near the composer. Mode changes are transcript events when they affect execution policy.

### 25.6 Specialized tool and artifact presentation

Generic tool rhythm is retained, with specialized summaries:

| Tool | Compact evidence |
|---|---|
| Read | Path and line range |
| Search | Query, scope, match count |
| Edit | Files and `+/-` line summary |
| Command | Command, exit code, duration, working directory |
| Web | Domain, title, retrieval time, citation count |
| MCP | Server, tool, trust source, duration |
| Subagent | Name, objective, progress, handoff state |
| Image/artifact | Type, dimensions/size, path, preview availability |

Ghostty may offer Quick Look or open-in-editor actions for artifacts, while the TUI retains a textual fallback and canonical path. Terminal image protocols are enhancements, never required for understanding.

## 26. Composer, CJK/IME, and Advanced Input

### 26.1 IME composition

IME composition is a distinct input state. Enter confirms a candidate while composition is active and must never send the prompt. The candidate window anchors to the caret where the platform permits; otherwise it anchors to the first composer line.

Cursor movement, selection, deletion, and wrapping operate on grapheme clusters and terminal cell width, not bytes or scalar values. Test coverage must include Chinese punctuation, mixed CJK/Latin text, emoji sequences, combining marks, and full-width characters.

### 26.2 Composer editing model

- Undo/redo spans typing, paste, attachment insertion, mention completion, and history restore.
- Drafts save per session after a short idle interval and on every view switch.
- Optional Emacs and Vi bindings apply only inside the composer and expose their current mode.
- Multiline selection, clipboard operations, Home/End semantics, and word navigation follow platform expectations.
- Bracketed paste prevents control-sequence interpretation.
- Large pastes are parsed off the render path and can be cancelled.

### 26.3 Mentions and templates

Composer completion recognizes:

- `@file` and `@folder` for workspace context.
- `@agent` for delegation or directed questions.
- `@session` for referencing prior work.
- `/command` for UI actions.
- Prompt templates with named parameters.

Completion inserts typed objects, not fragile display strings. Missing resources remain visible with an error state and replacement action.

### 26.4 Attachments and drag/drop

Files dropped into Ghostty become composer attachments after path, type, size, permission, and workspace-boundary validation. Directories become scoped references rather than recursively embedded content. Images show a small preview only when supported; the path and dimensions remain primary.

Before sending, the composer warns about very large context contribution, unsupported media, secrets detected in pasted text, and resources outside the permitted workspace.

### 26.5 Prompt history and draft conflicts

History search defaults to the current session and can expand to workspace or global scope. Restoring a prompt creates an undo point. If the same session has drafts from two clients, the composer offers compare, keep both, or choose one; it never silently overwrites.

## 27. Resilience, Operations, and Trust

### 27.1 Failure ownership

Failures are classified by owner so recovery actions are accurate:

| Owner | Examples | Recovery surface |
|---|---|---|
| TUI | render panic, terminal restore | Local recovery screen |
| Ghostty PTY | child exit, shell failure | Main session view with restart action |
| Server | crash, unavailable socket | Persistent reconnect banner and diagnostics |
| Provider | auth, rate limit, outage | Turn-level failure with model/provider options |
| Tool/MCP | timeout, malformed output | Tool row and server isolation controls |
| Storage | SQLite lock/corruption | Read-only recovery and backup/repair workflow |

### 27.2 Crash and reconnect

Terminal state restoration runs on normal exit, interrupt, and panic. After restart, the client reloads the last durable event cursor, reconciles incomplete tools, restores drafts, and labels uncertain external actions instead of replaying them automatically.

Reconnect backoff remains visible but quiet. Users can retry now, work on a draft offline, copy the draft, switch server, or open diagnostics.

### 27.3 Performance degradation

- High-frequency streaming is coalesced into perceptually smooth frames without losing events.
- Tool output over a threshold switches to a bounded live tail plus durable artifact.
- Long transcripts use semantic virtualization and retain search/index capability.
- Slow storage or network shows which subsystem is delayed.
- If rendering pauses, the UI says `output continues · display catching up` and reports buffered event count.
- Sidebar updates are batched so hundreds of sessions do not reorder beneath selection.

### 27.4 Diagnostics and safe mode

Diagnostics summarizes client/server versions, connection, session ID, terminal capabilities, renderer performance, storage health, provider state, and recent redacted errors. Users can copy a redacted bundle or save it explicitly.

Safe mode disables third-party MCP servers, custom shaders, skills, and nonessential plugins while preserving session access. The UI identifies which extension caused isolation when known.

### 27.5 Security and trust

The status/inspection system exposes workspace root, sandbox mode, network policy, approval mode, active skills, MCP provenance, and remote environment. Untrusted external content and prompt-injection risk are labeled near the affected tool evidence.

Secrets are masked in transcript, tool output, notifications, diagnostics, and exports. Revealing a secret is a deliberate local action that is never persisted in revealed form.

Completion claims distinguish:

- **Verified:** supporting command/test evidence exists.
- **Completed, unverified:** work ended without verification.
- **Partial:** requested scope remains.
- **Blocked:** external decision or unavailable dependency is required.

### 27.6 Updates and compatibility

Update notices are non-modal unless a protocol incompatibility prevents connection. Client and server negotiate protocol versions; incompatible combinations explain which component must update. Updates never interrupt an active turn and require an explicit restart boundary.

## 28. Gap Audit

The v4 report covers the intended product surface, but breadth alone does not make the design implementation-ready. The following gaps are the active design backlog and must be closed in priority order.

| Priority | Gap | Current risk | Required evidence |
|---|---|---|---|
| P0 | Report and artifact drift | The report references files that may not exist or are absent from the handoff index | Link check, numbered artifact inventory, and rendered contact sheet |
| P0 | End-to-end journeys are implicit | Individual states do not prove that entry, transition, recovery, and exit form a coherent flow | Journey maps for start/resume, plan-to-execute, approve, interrupt, reconnect, and fork |
| P0 | Execution and decision states overlap | Approval, AskUser, queue, steer, interrupt, and disconnect can compete for focus | One precedence table and deterministic focus/notification rules |
| P1 | Keyboard contract is distributed | Shortcuts can conflict across composer, overlay, transcript, shell, and IME | Context-keyed shortcut matrix with conflict resolution and discoverability behavior |
| P1 | State ownership is underspecified | TUI, Ghostty, and server may each appear to own draft, view, or execution state | Event/state ownership table including persistence and reconciliation |
| P1 | Responsive behavior lacks cell metrics | Visual mockups do not define exact collapse thresholds or minimum usable dimensions | Layout table at 120, 100, 80, 60, and 40 columns plus height constraints |
| P1 | Destructive recovery lacks full confirmation paths | Delete, revoke, revert, force-stop, and workspace escape can be misunderstood | Before/during/after/error states with explicit irreversible effects |
| P1 | Accessibility is mostly normative text | ASCII, monochrome, screen-reader, reduced-motion, and CJK behavior are not all shown | Paired fallback examples and test cases |
| P2 | Components are not mapped to protocol events | Implementation could invent inconsistent loading, terminal, and error states | Event-to-component matrix with payload, update, terminal, and retry behavior |
| P2 | Visual tokens are not terminal tokens | Pixel spacing and colors do not yet resolve to terminal cells and capability tiers | Ratatui-oriented cell, style, symbol, and color fallback tokens |
| P2 | Acceptance criteria are feature-level | They are difficult to replay as design QA scenarios | Given/When/Then scenario suite with artifact references |
| P3 | Real-content stress cases are thin | Long paths, large diffs, bursty streams, 300+ sessions, and mixed-width text may break hierarchy | Stress renders and truncation/wrapping rules using production-shaped content |

### 28.1 Interaction precedence

Only one blocking interaction owns keyboard focus. Lower-priority activity remains visible but cannot steal focus.

| Rank | State | Focus rule |
|---|---|---|
| 1 | Destructive approval or invalidated approval | Focus decision; preserve composer draft; `Esc` denies only after confirmation when execution consequences exist |
| 2 | AskUser required to continue | Focus answer control; queued prompts remain parked |
| 3 | Draft conflict or recovery choice | Focus reconciliation; never select a destructive default |
| 4 | Command palette, switcher, search, inspect | Temporary overlay; `Esc` returns to the exact prior focus and scroll anchor |
| 5 | Composer | Default focus when no blocking layer exists |
| 6 | Transcript navigation | Explicitly entered; streaming never steals focus or scroll position |

Connection loss does not dismiss ranks 1–3. Their actions become unavailable with a clear `reconnect to decide` status until server state is reconciled.

## 29. Iteration Plan

| Phase | Scope | Exit criteria | Status |
|---|---|---|---|
| P0 — Audit and consistency | Inventory report, artifacts, indexes, and interrupted work | All links resolve; XML and JSON validate; gaps are recorded | In progress |
| P1 — Missing interaction artifacts | Complete session, execution, permission, navigation, composer/IME, and resilience boards | Sections 22–27 each have editable visual evidence | In progress |
| P2 — Journeys and state contracts | Add primary journeys, precedence, focus, shortcut, ownership, responsive, and fallback matrices | Every primary journey has entry, transition, recovery, and exit | Pending |
| P3 — Implementation handoff | Define terminal-cell tokens, event/component mapping, persistence boundaries, and component contracts | Engineers can implement without inventing product behavior | Pending |
| P4 — Design verification | Render all boards; test links, XML/JSON, narrow widths, monochrome/ASCII, CJK, and scenario coverage | Verification report records pass/fail evidence and remaining risks | Pending |

Each phase updates this report first, then its editable artifacts and handoff index. A phase is not complete merely because prose exists; its exit evidence must be present and verified.

## 30. Version History

| Version | Date | Change |
|---|---|---|
| 4.1 | 2026-07-11 | Recorded the implementation-readiness gap audit, blocking-interaction precedence, and phased design iteration plan; synchronized the advanced design artifact set |
| 4.0 | 2026-07-11 | Added advanced session scale, execution control, Permission Center, search/checkpoint/fork, context/model/tool/artifact behavior, CJK/IME composer, resilience, diagnostics, security, multi-window, and performance specifications |
| 3.0 | 2026-07-11 | Replaced gray boxed transcript treatment with immersive canvas output, replaced Ghostty top tabs with a left session sidebar, added compact Sylvander brand mark, and split mockups by design level |
| 2.0 | 2026-07-11 | Added design synthesis, information architecture, Ghostty desktop detail, recovery/accessibility requirements, and editable design artifacts |
| 1.0 | 2026-07-11 | Approved initial TUI experience and Ghostty integration direction |
