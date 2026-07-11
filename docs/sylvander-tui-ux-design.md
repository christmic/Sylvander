# Sylvander TUI Experience Design

> Status: Approved baseline  
> Version: 1.0  
> Date: 2026-07-11  
> Scope: Terminal-native agent interface and its use inside the Sylvander Ghostty desktop shell

## 1. Decision

Sylvander has one canonical conversation interface: `sylvander-tui`.

The same TUI runs in two environments:

1. Directly in any compatible terminal.
2. In a PTY hosted by a Sylvander Ghostty tab.

Ghostty manages windows, tabs, PTYs, and process lifecycle. It does not implement a second native conversation UI and does not contain agent business logic. `sylvander-server` owns agents, tools, approvals, session history, and persistence.

```text
Normal terminal                  Sylvander Desktop
┌─────────────────────┐          ┌──────────────────────────┐
│ sylvander-tui       │          │ Ghostty window           │
│ one active session  │          │ ├─ tab: session A → TUI  │
└──────────┬──────────┘          │ ├─ tab: session B → TUI  │
           │                     │ └─ tab: session C → TUI  │
           │                     └────────────┬─────────────┘
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

## 3. Evidence from the Current Repository

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

## 4. Canonical Conversation Screen

The reference viewport is 120 columns by 36 rows.

```text
┌─ Sylvander 🦀 ───────────────────────────────────────────────────────────────┐
│ auth-refactor                                            claude-sonnet-5  ▾ │
│ ~/Projects/acme-api · feat/auth-refactor · session 8f21                    │
└─────────────────────────────────────────────────────────────────────────────┘

  You
  Add JWT authentication. First inspect the existing middleware and propose
  a plan before changing anything.

  Oraculo
  I’ll inspect the authentication flow and identify where token validation
  belongs.

  ● Exploring the codebase                                            12s
    ├─ ✓ Read  src/http/router.rs
    ├─ ✓ Search "middleware" in src/
    ├─ ✓ Read  src/auth/mod.rs
    └─ ◐ Inspecting tests/auth_test.rs

  The project already has session-cookie authentication in `src/auth/mod.rs`.
  JWT support can share its identity extraction layer.

  Proposed plan

  1. Extract `AuthenticatedUser` from the existing cookie middleware.
  2. Add a JWT verifier implementing the same authentication interface.
  3. Register both mechanisms in the HTTP authentication layer.
  4. Add unit and integration coverage.

  No files have been changed yet.

╭─────────────────────────────────────────────────────────────────────────────╮
│ Ask Sylvander…                                                              │
│                                                                             │
╰─────────────────────────────────────────────────────────────────────────────╯
  normal · plan mode   context 24%   3 tools   main
  ↵ send   ⇧↵ newline   esc interrupt   ctrl+r history   / commands   ? help
```

### 4.1 Header

The header occupies at most three rows and shows:

- Session name.
- Workspace path.
- Git branch when available.
- Selected model.
- Short session identifier.

Secondary metadata collapses before it wraps. The header must never displace the composer.

### 4.2 Transcript

The transcript is semantic rather than a raw event log. It contains user messages, assistant messages, execution groups, plans, errors, and decisions.

Rules:

- Assistant streaming updates one live block in place.
- Completed streaming content becomes one transcript entry.
- Thinking is collapsed by default and must be visually distinct.
- Tool events belonging to one agent step are grouped.
- Successful low-information operations remain one line.
- Failures remain visible until acknowledged or superseded.
- The viewport follows the bottom unless the user scrolls upward.
- New output must not steal the viewport while the user reads history.

### 4.3 Composer

The composer is multiline and remains anchored above the status rows.

Required behavior:

- `Enter` sends.
- `Shift+Enter` inserts a newline.
- Arrow keys edit text when the composer owns focus.
- `Ctrl+R` searches prompt history.
- `/` opens the command palette when entered at the start of an empty prompt.
- Pasted multiline content is preserved and visually bounded.
- Large pastes show a compact attachment-like summary with an expand action.
- Draft text survives modal interactions and session switching.

## 5. Active Execution

Tool work is grouped into one live execution block.

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

╭─────────────────────────────────────────────────────────────────────────────╮
│ You can type while I work. Your message will steer the current turn…       │
╰─────────────────────────────────────────────────────────────────────────────╯
  working · esc interrupt   shift+tab switch mode   ctrl+t tasks
```

The composer stays usable during execution. A submitted message can:

- Steer the current turn.
- Queue the next instruction.
- Interrupt and replace the current instruction.

When intent is ambiguous and the distinction matters, Sylvander presents those three choices rather than guessing.

## 6. Tool Presentation

Routine results are compact:

```text
  ✓ Read src/auth/mod.rs                                  126 lines
  ✓ Edited src/auth/jwt.rs                              +48  -3
  ✓ cargo test auth                                      18 passed
```

Selected items expand in place:

```text
  ▾ Edited src/auth/jwt.rs                              +48  -3
    ┌─────────────────────────────────────────────────────────────────────┐
  41│+ pub struct JwtVerifier {
  42│+     decoding_key: DecodingKey,
  43│+     validation: Validation,
    │
  67│- validate_session_cookie(cookie)
  67│+ authenticate_request(request).await
    └─────────────────────────────────────────────────────────────────────┘
```

Long command output is collapsed by default:

```text
  ✓ cargo test --workspace
    138 passed · 0 failed · completed in 8.4s
    [enter to expand output]
```

Tool display order is stable: state icon, action, target, result summary, duration. Color reinforces state but never replaces the icon or label.

## 7. Approval Request

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

## 8. Plan Mode

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

## 9. Sessions

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

In a normal terminal, opening a session replaces the current view. In the Sylvander Ghostty desktop, the shell may intercept an explicit open-in-new-tab action. Closing a tab stops its TUI process but does not delete the persisted session.

Session state values are `working`, `waiting`, `complete`, `failed`, and `disconnected`. Destructive deletion always requires confirmation.

## 10. Tasks and Subagents

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

## 11. Command Palette

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

## 12. Responsive Layout

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
- Narrow: below 80 columns; single-column cards and minimal status.
- Minimum supported viewport: 50 columns by 12 rows.
- Below the minimum, show a clear resize message without corrupting terminal state.

## 13. Focus and Navigation

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

## 14. Visual Language

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

## 15. Ghostty Desktop Integration Contract

Each Sylvander Ghostty agent tab owns a PTY child process running `sylvander-tui` with an explicit session identity and workspace.

Conceptual launch forms:

```text
sylvander-tui --new --workspace <path>
sylvander-tui --session <session-id>
```

The native shell may:

- Create and close tabs.
- Restore tabs by persisted session identifier.
- Reflect session state in tab titles and indicators.
- Open a selected session in a new tab.
- Request a graceful TUI shutdown before terminating the PTY.

The native shell must not:

- Render assistant messages or tool calls itself.
- Implement approvals or agent modes.
- Own conversation history.
- Duplicate session or protocol state already owned by the server.

## 16. Acceptance Criteria

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
- The exact same TUI binary works in regular terminals and Sylvander Ghostty tabs.
- Closing a Ghostty tab does not delete its session.
- Terminal state is restored after normal exit, panic, disconnect, and interrupt.

## 17. Delivery Sequence

1. Stabilize shared protocol and persistent session semantics.
2. Build the transcript viewport and semantic event model.
3. Replace single-line input with the multiline composer.
4. Add stable streaming and grouped tool presentation.
5. Add approvals, plan mode, interruption, and steering.
6. Add session switcher, history restoration, and reconnect.
7. Add commands, tasks, subagent visibility, and context status.
8. Verify responsive behavior and terminal compatibility.
9. Integrate the unchanged TUI binary into Ghostty PTY tabs.
10. Add desktop tab restoration and session-state indicators.

## 18. Non-Goals for the Baseline

- A separate SwiftUI conversation renderer.
- Agent execution or tool logic inside Ghostty.
- Pixel-identical rendering across terminal fonts.
- Mouse-only interactions.
- A permanently visible session sidebar.
- Rich GUI widgets that cannot degrade to terminal cells.

## 19. Version History

| Version | Date | Change |
|---|---|---|
| 1.0 | 2026-07-11 | Approved initial TUI experience and Ghostty integration direction |
