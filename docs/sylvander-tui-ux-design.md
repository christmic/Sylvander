# Sylvander TUI UI and Interaction Specification

> Status: normative UI source of truth
>
> Scope: `sylvander-tui` visual layout and direct terminal interaction
>
> Updated: 2026-07-13

This document defines what Sylvander looks and feels like in a terminal. It does
not define Agent control flow, public protocol events, Token9, or Ghostty native
window management.

## 1. Product experience

Sylvander is an immersive terminal Agent: calm, capable, companionable, and
technically serious. The interface establishes the Seed-Crab character once,
then lets the work dominate.

The conversation surface intentionally follows the familiar Claude Code rhythm
where learned terminal behavior matters. The verified visual baseline for this
revision is Claude Code 2.1.197, inspected in a real 80 x 24 PTY on 2026-07-13.
Sylvander keeps its own identity, state model, runtime, protocol, and tools.

The design sources are:

- Claude Code: content remains dominant and ordinary prose is quiet.
- Codex: input, status, and live execution remain legible under sustained work.
- Qwen Code: terminal identity is authored, not produced by automatic image
  conversion.
- Kimi Code: a small recurring presence can carry identity without becoming
  decoration.

### 1.1 Competitive synthesis

Sylvander reproduces Claude Code's quiet transcript grammar closely enough that
users do not need to relearn message and tool hierarchy. Product identity stays
in the Seed-Crab Welcome, palette, status semantics, and Sylvander-specific
surfaces rather than in novel conversation chrome.

| Reference | Keep | Improve for Sylvander |
|---|---|---|
| Claude Code | Quiet prose, natural-language decisions, uninterrupted terminal rhythm | Stronger authored identity and clearer semantic state |
| Codex | Legible live work, concise approval choices, settled decision history | Remove gray/card-like Composer chrome and keep the canvas immersive |
| Kimi Code | Tool-specific previews, inline feedback, structured multi-question flow | Reduce persistent density and keep ordinary work visually quiet |
| Sylvander existing TUI | Seed-Crab Welcome, pure-black canvas, left-anchored transcript, full-width Composer | Replace generic centered popups with native terminal surfaces |

The result must retain Claude's low-friction reading rhythm, feel calmer than
Kimi, warmer than Codex, and remain internally consistent with Sylvander's own
Welcome and semantic state.

## 2. Visual foundation

### 2.1 Canvas

- Background is pure black `#000000`.
- There is no gray transcript panel, message card, or permanent outer frame.
- Warm ivory `#ECE7DE` is primary text.
- Gray is reserved for metadata, hints, rules, and inactive state.
- Seed-Crab warm gold `#F0BE72` and core violet `#9B72FF` are identity colors.
- Blue means active work, teal verified, amber waiting, red failure/destruction.

### 2.2 Typography

The TUI inherits the user's monospace terminal font. Hierarchy comes from weight,
color, spacing, and alignment—not simulated block lettering.

- `Sylvander`: mixed case, bold, warm ivory.
- `agent workspace`: violet, italic where supported.
- Body: regular warm ivory.
- Metadata labels: muted gray; values: primary text.
- Raw Markdown decoration such as `**` and backticks must not leak into rendered
  prose. Semantic emphasis may use terminal bold or color.

## 3. Stable screen structure

From top to bottom:

1. Transcript, including the Welcome prelude and conversation turns.
2. Full-width Composer.
3. Temporary interaction region, present only while a decision or picker owns
   focus.
4. One-row status line.

There is no permanent top Header. Model, branch, session, tool count, mode, and
contextual shortcuts belong in the bottom status line.

### 3.1 Single-session boundary

The standalone TUI displays exactly one active session. It never owns a
persistent session sidebar, split conversation view, or simultaneous session
presence model.

- `/resume` and `/sessions` open a temporary picker and replace the currently
  loaded session only after the user selects one and the service acknowledges it.
- Leaving that picker restores the same transcript, Composer draft, and scroll
  position.
- A Ghostty-based host may run several independent TUI processes and provide its
  own session sidebar. That host navigation is outside this document and must
  not leak into the standalone TUI.

## 4. Horizontal layout

- Transcript and Welcome use a maximum reading width of 110 cells.
- Their left edge is fixed at terminal column 1, matching Claude's transcript
  and the live Composer prompt.
- Widening or fullscreening a terminal never recenters this column.
- Extra fullscreen width remains empty on the right.
- Composer separators and status span the entire terminal width.
- Below the minimum supported viewport, show an explicit resize state rather
  than corrupting layout.

## 5. Welcome as transcript prelude

Welcome is not a disposable splash page. It is the first content block in the
conversation.

### 5.1 Standard and wide layout

The approved 11-row terminal Seed-Crab appears on the left. On the right:

```text
Sylvander
agent workspace

model      <model or —>
workspace  <compact path>
branch     <branch or —>
session    <new or short id>

What should we work through?
```

- Character column: 44 cells.
- Gap: 4 cells.
- Horizontal layout begins at 88 available cells.
- The full character includes sprout, split shell, paired core lights, both
  claws, and lower walking legs.

### 5.2 Lifecycle

- Before input, Welcome is visible at the top of the transcript.
- On submit, the user turn is appended below Welcome.
- Agent output is appended below the user turn.
- Welcome is never immediately cleared or replaced.
- Welcome inclusion is unconditional for a session. Connection diagnostics,
  cached session metadata, and existing message rows append after it; they are
  never used as predicates for whether it exists.
- Opening a command picker, approval, question, or any other temporary surface
  never changes whether Welcome belongs to the transcript. These surfaces may
  reduce its visible viewport temporarily, but closing them restores the same
  prelude and scroll position.
- When the conversation exceeds the viewport, normal live-follow scrolling
  moves the oldest lines—including Welcome—off screen.
- Clearing a transcript may reveal a fresh Welcome prelude again.

### 5.3 Narrow layout

The same canonical character is used. Information reflows below it. There is no
alternative mascot, reduced logo, or automatic ASCII fallback posing as a
different identity.

## 6. Conversation rhythm

### 6.1 User turn

```text
❯ What tools do you have?
```

- `❯` uses dim bold text and matches the Composer prompt vocabulary.
- User text uses primary text.
- A submitted user turn has no surrounding rules and no hardware cursor. Only
  the bottom live Composer owns those affordances, so history cannot appear to
  be a second active input surface.
- A blank row separates meaningful turns.

### 6.2 Agent turn

```text
⏺ I have the following tools available:

  1. ask_user — Ask for a decision or missing information.
  2. Read — Read a file inside the workspace.
  3. Write — Create or replace file content.
```

- `⏺` is a compact violet presence mark shown once per meaningful Agent turn.
- It is not a fallback Logo and does not replace the full Seed-Crab.
- Continuation lines align two cells after the transcript origin.
- The former `/\\`, `(••)`, `<__>` reply face is prohibited.
- Prose wraps at word boundaries. It must not split ordinary words merely to
  fill the final cell.
- Explicit paragraphs remain explicit.
- Numbered and bulleted lists receive line breaks and hanging indentation.
- Consecutive paragraphs do not repeat the presence mark.

### 6.3 Streaming

Streaming uses the same final typography. Partial output occupies one live Agent
turn and settles in place; completion must not redraw it into a visibly different
shape.

### 6.4 Thinking and tools

- Thinking is subdued, compact, and removed or collapsed when final prose starts.
- Tool activity is inline. A primary activity uses `⏺`; semantic color and
  explicit failure copy carry state without adding a second hierarchy of icons.
- Child tools use one quiet `⎿` relationship marker instead of persistent
  vertical chrome or an additional status glyph.
- Completed Edit and Write operations show a bounded unified diff by default;
  additional context remains explicitly expandable.
- Routine tool output has no filled container.
- Long tool details appear only when inspected.

## 7. Composer

### 7.1 Resting state

```text
────────────────────────────────────────────────────────────────
❯
────────────────────────────────────────────────────────────────
```

- Both rules span the full terminal width.
- `❯` starts at column 1, touching the same left edge as the rules.
- There is no left border, gray box, or `Ask Sylvander…` placeholder.
- Empty Composer content height is one row.

### 7.2 Editing

- Typed text follows `❯ `.
- The Composer grows upward as text wraps or explicit newlines are inserted.
- Maximum visible draft height is eight rows; further content scrolls internally.
- Continuation rows align with text after the prompt.
- Enter sends; Shift+Enter inserts a newline.
- While the Composer owns focus, the hardware cursor is visible after the `> `
  prompt even when the draft is empty.
- The hardware cursor follows the logical cursor after `❯ ` by terminal-cell
  width and never
  enters the status row. CJK glyphs, emoji, and combining sequences must not
  displace it.

## 8. Status line

The status line is always bottommost and single-row where width permits.

Left side:

```text
· idle · model MiniMax-M3 · branch master · session — · 0t
```

Right side shows no more than two or three contextual actions, such as:

```text
↵ send   ⇧↵ newline
```

At narrow widths, low-priority metadata disappears before primary state.

## 9. Scrolling and resize

- `↑` and `↓` belong to the Composer for draft movement and submitted-input
  history; they do not scroll the transcript.
- Mouse-wheel up/down belongs exclusively to the transcript and never changes
  Composer history or cursor position.
- Live-follow keeps the newest transcript content visible.
- PageUp detaches; incoming output does not move the user's reading position.
- PageDown or Ctrl+End returns to live output.
- An unread indicator appears only while detached.
- Resize preserves conversation order, Composer draft, and scroll intent.
- Fullscreen width never changes the transcript's left anchor.

## 10. Temporary interaction surfaces

Temporary interaction must feel like a continuation of the terminal session,
not a desktop dialog placed over it. Generic centered rectangles are prohibited.
Every temporary surface belongs to one of three families.

### 10.1 Decision Dock

The Decision Dock handles approvals, Agent questions, plan acceptance, rollback
confirmation, and other short decisions.

- It is inserted below the visible Composer and above the bottom status row; the
  saved draft remains visible but does not receive input.
- It shares the Composer's bottom rule as its top boundary and adds only one
  full-width bottom rule.
- Decision content is left-aligned with the transcript and capped at 110 cells.
- The Composer `❯` remains visible, but its hardware cursor is hidden while the
  Dock owns focus, so there is only one active input target.
- One question or approval is shown at a time. A batch uses quiet progress such
  as `2 of 3`, not redundant queue terminology.
- Choice labels use natural language. Protocol types such as `single-select`,
  `multi-select`, `call_id`, and `batch` are not product copy.
- The safest action is visibly identified for critical operations and may own
  the initial selection. Other risk levels default according to policy without
  disguising the consequence.
- The status row keeps no more than three contextual hints.

Approval information order is fixed:

1. What Sylvander wants to do.
2. The exact target or command.
3. Why attention is required and what can change.
4. Available scopes expressed in plain language.
5. Optional rejection guidance inline with the selected reject action.

An accepted or rejected decision settles into one compact transcript row. The
Dock itself never becomes transcript history.

Agent questions use the same Dock. A single question shows options and an
`Other…` choice. Selecting `Other…` turns that row into an inline editor. Multiple
questions appear one at a time with `Question 1 of 3`; only multi-question flows
receive a final answer review.

### 10.2 Focus Picker

The Focus Picker handles commands, model selection, permission profiles,
workspace-file mentions, and persisted-session selection.

- It is inserted below the Composer instead of floating in the center.
- Results appear first, followed by one filter-query row and a bottom rule. The
  visible heading is omitted when the result rows already communicate the
  surface purpose.
- Commands and the query share the viewport's left baseline; selection uses one
  concise marker rather than nested popup indentation.
- At standard width it shows 6–10 results. At narrow width it uses fewer rows and
  removes secondary descriptions before truncating primary labels.
- Selection, current value, availability, and consequences have distinct visual
  roles. Disabled entries remain visible with one concise reason.
- `/resume` is a picker for replacing the one active TUI session. It does not
  represent simultaneous sessions and never becomes a sidebar.
- Escape closes the picker and restores the exact draft and transcript position.

### 10.3 Review View

The Review View handles content that must be read before deciding: a long plan,
diff, file preview, or rollback scope.

- It temporarily uses the transcript viewport rather than covering it with a
  centered box.
- Its top line names the review object and shows compact progress or file scope.
- Review content may use more horizontal width than prose when code or diffs need
  it, but it remains left anchored.
- Search, jump, expand, and scroll hints appear only while the view owns focus.
- The final accept/revise/cancel decision appears in a Decision Dock.
- A plan already rendered in the transcript is not duplicated. Initial plan
  acceptance uses a Decision Dock; Review View opens only for explicit editing.
- Closing the view restores the transcript, Composer draft, and scroll intent.

### 10.4 Focus and restoration

Only the top temporary surface receives keyboard input. Mouse wheel and page keys
scroll that surface while it owns focus. Global quit, history navigation, and
Composer editing cannot leak through. Timeouts or server cancellation close the
matching surface immediately and leave a concise explanation in the transcript.

Every surface must render cleanly without translucency, shadows, filled gray
cards, or terminal-background assumptions. Color adds semantic hierarchy but is
never the sole carrier of risk, selection, or state.

## 11. Responsive matrix

| Available width | Welcome | Transcript | Temporary surface | Status |
|---|---|---|---|---|
| 110+ | Character left, information right | 110-cell max, left anchored | Full detail, left anchored | Full |
| 88–109 | Character left, information right | Available width | Full detail | Full/compact |
| 50–87 | Same character, information below | Available width | Wrapped choices, reduced metadata | Compact |
| <50 | Resize state | Not rendered | Not rendered | Not rendered |

## 12. Prohibited UI

- Centering the capped transcript in fullscreen.
- Clearing Welcome immediately after the first submit.
- Rendering replies with `/\\ (••) <__>`.
- Raw `**bold**` or backtick markers in ordinary output.
- Character-level prose wrapping that splits normal words.
- Gray transcript cards or a permanent bordered Composer.
- Generic centered modal rectangles for decisions or pickers.
- Two simultaneous hardware cursors or two surfaces that both appear editable.
- Implementation labels such as `single-select`, `batch`, or `call_id` in UI copy.
- Duplicating an inline plan inside a second review popup.
- A permanent or temporary session sidebar inside the standalone TUI.
- Top tabs for Ghostty sessions.
- A model/session/header strip above the transcript.
- A different or simplified mascot at narrow widths.

## 13. Visual acceptance checks

A change is acceptable only when all are true:

1. Welcome, first user turn, and first Agent response are simultaneously visible
   in a sufficiently tall fresh session.
2. A 240-column viewport keeps transcript origin at column 1.
3. `❯` begins at column 1 and both Composer rules span the viewport.
4. A long numbered answer wraps on words and shows no raw Markdown markers.
5. The Agent presence mark occurs once per turn.
6. Streaming and settled output keep the same geometry.
7. 70-column layout uses the same Welcome character above metadata.
8. Pure black remains visible in unused space.
9. Approval follows the visible Composer, leaves the transcript readable, keeps
   status bottommost, and exposes exactly one apparent focus.
10. AskUser, plan acceptance, and rollback confirmation share Decision Dock
    geometry without sharing inappropriate copy.
11. Command, model, permission, and resume pickers rise from the bottom and never
    appear as centered desktop dialogs.
12. `/resume` never suggests that more than one session is active in the TUI.
13. Closing any temporary surface restores the exact draft and scroll position.
14. During tool activity, historical `❯` turns remain unframed; exactly one
    pair of full-width rules and one hardware cursor identify the live Composer.

## 14. Editable references

The current editable boards are indexed by `docs/design/README.md`. Boards 04–07
define the temporary interaction surfaces in this section. Old rejected brand
explorations have no normative authority.
