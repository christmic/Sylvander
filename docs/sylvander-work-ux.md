# Sylvander Work interaction specification

> Status: normative foundation specification
>
> Updated: 2026-08-13

## Product experience

Sylvander Work is the calm desktop control surface for durable Agent work. It makes
parallel Sessions, live execution, decisions, and verification easy to scan
without turning the product into an operations dashboard.

The desktop keeps the TUI's quiet transcript, warm ivory text, Seed-Crab gold,
and core violet. It adds spatial navigation and progressive detail where a
window provides room. Ordinary messages remain unboxed; cards are reserved for
decisions or structured evidence.

## Information architecture

The primary window contains three persistent regions at wide widths:

```text
┌────────┬──────────────────┬──────────────────────────────┬──────────────┐
│ rail   │ Sessions         │ conversation                 │ inspector    │
│        │                  │                              │ optional     │
│ home   │ search / new     │ Session context              │ plan         │
│ work   │ active           │ transcript                   │ tasks        │
│ agents │ recent           │ decision dock                │ changes      │
│ config │ archived         │ Composer                     │ context      │
└────────┴──────────────────┴──────────────────────────────┴──────────────┘
```

- The 64 px rail changes product surfaces. It never becomes a second Session
  selector.
- The 280 px Session sidebar owns search, new Session, recent Sessions, and
  archived access.
- Conversation is the only region that grows without a fixed maximum width.
- The 320 px inspector is contextual and opens only for plan, tasks, changes,
  context, or detailed tool output.
- Runtime health is visible but quiet. It becomes prominent only while
  connecting, degraded, or offline.

## Responsive behavior

| Window width | Layout |
|---|---|
| 1180 px and above | Rail, Session sidebar, conversation, optional inspector |
| 760–1179 px | Rail, conversation, one overlay drawer for Sessions or inspector |
| Below 760 px | Compact top bar, conversation, bottom Composer; navigation and inspector are full-height drawers |

The main conversation never shrinks below a readable 440 px while another
region remains docked. Resizing preserves draft text, active decision, selected
Session, transcript scroll, and inspector selection.

## Session navigation

Each Session row shows only label, compact workspace, recency, and a semantic
presence dot. It does not invent unread counts or completion state absent from
the Runtime protocol.

- Selecting a Session loads its Runtime history and restores its local draft.
- A running Session remains visible while another Session is selected.
- New Session first asks for Agent and workspace; no model work starts until
  the user submits a goal.
- Rename, archive, restore, fork, and delete are secondary row actions.
- Delete requires an explicit destructive confirmation and never shares the
  primary click target.
- Search filters already-loaded labels and workspaces immediately; server-side
  discovery may extend it later.

`Command/Ctrl+K` opens a keyboard Session switcher. `Command/Ctrl+N` starts a
new Session. `Command/Ctrl+Shift+[` and `]` move through recent Sessions.

## Conversation

Conversation follows the TUI grammar in a wider medium:

- User turns use a quiet `❯` lead and no bubble.
- Agent turns use one violet presence mark and readable prose.
- Thinking is collapsed by default after final text begins.
- Tool activity appears inline as a compact timeline; input and output expand
  on request.
- Long output, diffs, and artifacts open in the inspector without moving the
  reader away from the turn that produced them.
- Streaming text settles in place. Completion does not replace the live row
  with a visually unrelated component.

The transcript uses a 760 px reading column aligned to the left of the main
content region. It does not center itself within the entire application window.

## Composer

The Composer is visually stable at the bottom of the conversation. It includes:

- a multiline text field that grows to eight lines;
- explicitly selected UTF-8 files, with PNG/JPEG enabled only for the exact
  provider-qualified model that advertises vision;
- the selected model, reasoning effort, and permission profile as compact
  controls;
- Send while idle and Interrupt while the selected Session is running;
- a concise shortcut hint that disappears at narrow widths.

`Enter` sends and `Shift+Enter` inserts a line break. Input remains responsive
while streaming. A text-empty draft may send selected attachments; a fully
empty draft is ignored. Drafts are local presentation state and are never
reported as persisted Session content. Attachment errors name only the public
file name and limit failure; they never expose file paths or content.

Desktop settings offer an opt-in “Notify when background turns finish” switch.
The switch changes only after the native host confirms persistence. When
enabled, an unfocused window may produce a fixed completed, failed, or
interrupted notification from the matching Runtime terminal; no conversation
content or Session identity appears in the operating-system surface. Focused
work never produces a duplicate notification.

## Decisions and questions

Approval, AskUser, and plan review use an anchored Decision Dock immediately
above the Composer. Generic centered dialogs are not used for Agent decisions.

- The dock names the exact Session and subject.
- Approval lists every tool in the batch and the allowed Runtime scopes.
- The default action is the least-authorizing choice.
- Reject accepts an optional reason and never masquerades as a tool failure.
- AskUser preserves option order and supports typed input where the protocol
  allows it.
- Plan review shows the complete ordered plan and explicit accept, revise, or
  reject actions.
- Only one dock receives focus. Additional decisions remain queued and visible
  as a count.

Destructive application actions such as deleting a Session use a native-style
confirmation sheet because they are not Agent interaction events.

## Plan, tasks, and changes

The inspector provides four tabs only when data exists:

- Plan shows ordered steps, current position, and settled review decision.
- Tasks show owner, purpose, progress, terminal status, and cancellation.
- Changes render the Runtime-owned coding-session diff and accept/discard
  actions; the desktop never reads Git directly.
- Context shows Runtime's context report, token usage, and compaction actions.

Terminal status uses both text and color. Verified completion is teal, active
work blue, waiting amber, and failure red. A green or teal mark is never shown
before the corresponding Runtime terminal event.

## Connection states

The application has five explicit connection states:

| State | Presentation | Allowed action |
|---|---|---|
| Starting | Native window and connection-safe skeleton | None |
| Connecting | Small activity label in title region | Cancel or wait |
| Live | Quiet teal Runtime indicator | Normal work |
| Reconnecting | Existing state remains readable; sending pauses | Retry now |
| Offline | Persistent explanation and endpoint-safe diagnostics | Reconnect or open settings |

A disconnect never clears the transcript, settles a running turn, or discards a
draft. Reconnection reattaches the selected Session before enabling Send.

## Visual system

Desktop tokens derive from the current brand source of truth:

- canvas `#000000`;
- elevated surface `#0C0B0F` and interactive surface `#15131A`;
- primary text `#ECE7DE` and muted text `#908A82`;
- Seed-Crab gold `#F0BE72` and core violet `#9B72FF`;
- active blue `#64A8FF`, verified teal `#55C8B0`, waiting amber `#E3A94F`,
  and failure red `#EF6A6A`.

Body text uses the platform UI font. Code, paths, model identifiers, and tool
output use a platform monospace stack. Corners are restrained at 8–14 px;
shadows are reserved for drawers and transient menus. Motion is functional,
under 180 ms, and disabled by `prefers-reduced-motion`.

## Accessibility

- Every control has a programmatic name and visible focus indicator.
- Landmarks identify navigation, Session list, conversation, inspector, and
  Composer.
- Streaming prose is not announced token by token. A polite live region
  announces meaningful status changes and terminal outcomes.
- Tool timelines, plans, and tasks expose text equivalents independent of
  color or icons.
- Focus moves into an opened Decision Dock and returns to the Composer after a
  decision settles.
- Escape closes only the top transient surface; it never interrupts work.
- The layout remains usable at 200% zoom and 320 CSS px viewport width.
- Contrast targets WCAG 2.2 AA for text and interactive components.

## Foundation acceptance scenarios

1. Launch into a useful window with visible Runtime state and Session list.
2. Switch between Sessions without losing independent drafts or scroll state.
3. Submit a goal, display streaming work, and settle on a terminal event.
4. Inspect a tool result without leaving the conversation.
5. Resolve an approval using only the keyboard and restore Composer focus.
6. Follow plan and task progress while reading a different Session.
7. Disconnect during streaming, retain honest state, reconnect, and reattach.
8. Collapse from wide to compact layout without losing interaction state.
9. Use reduced motion, high zoom, and visible keyboard focus throughout.
