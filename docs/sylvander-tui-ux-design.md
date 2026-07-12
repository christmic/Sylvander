# Sylvander TUI UI and Interaction Specification

> Status: normative UI source of truth
>
> Scope: `sylvander-tui` visual layout and direct terminal interaction
>
> Updated: 2026-07-12

This document defines what Sylvander looks and feels like in a terminal. It does
not define Agent control flow, public protocol events, Token9, or Ghostty native
window management.

## 1. Product experience

Sylvander is an immersive terminal Agent: calm, capable, companionable, and
technically serious. The interface establishes the Seed-Crab character once,
then lets the work dominate.

The design borrows useful principles rather than visual forms:

- Claude Code: content remains dominant and ordinary prose is quiet.
- Codex: input, status, and live execution remain legible under sustained work.
- Qwen Code: terminal identity is authored, not produced by automatic image
  conversion.
- Kimi Code: a small recurring presence can carry identity without becoming
  decoration.

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
3. One-row status line.

There is no permanent top Header. Model, branch, session, tool count, mode, and
contextual shortcuts belong in the bottom status line.

## 4. Horizontal layout

- Transcript and Welcome use a maximum reading width of 110 cells.
- Their left edge is fixed at terminal column 3: a two-cell gutter.
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
›  What tools do you have?
```

- `›` uses dim bold text.
- User text uses primary text.
- A blank row separates meaningful turns.

### 6.2 Agent turn

```text
◆  I have the following tools available:

   1. ask_user — Ask for a decision or missing information.
   2. Read — Read a file inside the workspace.
   3. Write — Create or replace file content.
```

- `◆` is a compact violet presence mark shown once per meaningful Agent turn.
- It is not a fallback Logo and does not replace the full Seed-Crab.
- Continuation lines align three cells after the transcript origin.
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
- Tool activity is inline and uses semantic state symbols.
- Routine tool output has no filled container.
- Long tool details appear only when inspected.

## 7. Composer

### 7.1 Resting state

```text
────────────────────────────────────────────────────────────────
>
────────────────────────────────────────────────────────────────
```

- Both rules span the full terminal width.
- `>` starts at column 1, touching the same left edge as the rules.
- There is no left border, gray box, or `Ask Sylvander…` placeholder.
- Empty Composer content height is one row.

### 7.2 Editing

- Typed text follows `> `.
- The Composer grows upward as text wraps or explicit newlines are inserted.
- Maximum visible draft height is eight rows; further content scrolls internally.
- Continuation rows align with text after the prompt.
- Enter sends; Shift+Enter inserts a newline.
- The hardware cursor follows the logical cursor and never enters the status row.

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

- Live-follow keeps the newest transcript content visible.
- PageUp detaches; incoming output does not move the user's reading position.
- PageDown or Ctrl+End returns to live output.
- An unread indicator appears only while detached.
- Resize preserves conversation order, Composer draft, and scroll intent.
- Fullscreen width never changes the transcript's left anchor.

## 10. Overlays

Approval, AskUser, session switching, commands, and plan review may overlay the
transcript. Overlays can use borders because they are temporary decision surfaces.
Closing an overlay restores the exact transcript and Composer underneath.

## 11. Responsive matrix

| Available width | Welcome | Transcript | Status |
|---|---|---|---|
| 110+ | Character left, information right | 110-cell max, left anchored | Full |
| 88–109 | Character left, information right | Available width | Full/compact |
| 50–87 | Same character, information below | Available width | Compact |
| <50 | Resize state | Not rendered | Not rendered |

## 12. Prohibited UI

- Centering the capped transcript in fullscreen.
- Clearing Welcome immediately after the first submit.
- Rendering replies with `/\\ (••) <__>`.
- Raw `**bold**` or backtick markers in ordinary output.
- Character-level prose wrapping that splits normal words.
- Gray transcript cards or a permanent bordered Composer.
- Top tabs for Ghostty sessions.
- A model/session/header strip above the transcript.
- A different or simplified mascot at narrow widths.

## 13. Visual acceptance checks

A change is acceptable only when all are true:

1. Welcome, first user turn, and first Agent response are simultaneously visible
   in a sufficiently tall fresh session.
2. A 240-column viewport keeps transcript origin at column 3.
3. `>` begins at column 1 and both Composer rules span the viewport.
4. A long numbered answer wraps on words and shows no raw Markdown markers.
5. The Agent presence mark occurs once per turn.
6. Streaming and settled output keep the same geometry.
7. 70-column layout uses the same Welcome character above metadata.
8. Pure black remains visible in unused space.

## 14. Editable references

The current editable boards are indexed by `docs/design/README.md`. Old numbered
boards and rejected brand explorations are intentionally removed and have no
normative authority.
