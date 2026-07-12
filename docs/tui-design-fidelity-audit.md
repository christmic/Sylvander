# TUI design fidelity audit — current build vs SVG ground truth

> **Author note**: I was reading the markdown design doc (`docs/sylvander-tui-ux-design.md`) instead of the SVG mockups under `docs/design/*.svg`. The user pointed out: "UI 的 TUI 都在 SVG 里面". The SVGs are the ground truth; the markdown is commentary on top.
> This audit codifies what the SVGs **require** vs what I currently **render**.
>
> **Brand supersession note (2026-07-12):** this audit predates the v3 anthropomorphic character specification. Its `◖S◗` references and SVG brand marks are historical implementation evidence, not current brand requirements. Interaction layout remains applicable; brand decisions defer to `docs/sylvander-brand-system.md`.

## How to read this

Each row cites the SVG mockup it derives from (`docs/design/<n>-<title>.svg`) and lists the visual primitives I missed in the current build. An `SVGs` citation is binding; an `§n.m` markdown citation is supporting context.

The audit finds roughly **8 major gaps**. M-T1..M-T13 covered interaction wiring; M-T14 is the visual rebuild.

---

## 1. Color palette (GLOBAL — breaks everything else until fixed)

### SVG ground truth (verbatim from `02-tui-immersive.svg` `<style>` block)
```css
.bg     #111315    /* warm-neutral dark canvas (NOT pure black)        */
.t      #ECE7DE    /* soft ivory primary text (NOT harsh white)       */
.h      #ECE7DE    /* bold ivory for headers                          */
.s      #989B9D    /* secondary metadata (model name, file path…)    */
.x      #666C72    /* tertiary muted (help text, sub-labels)           */
.a      #E8796A    /* CORAL — Sylvander identity, plan ◐, focus stroke */
.b      #75A7E8    /* blue — active work, "running…"                   */
.g      #72C7B1    /* teal — verified success, ✓                       */
.y      #D9AF62    /* amber — warning, waiting, deciding              */
.rule   #343A40    /* hairline separator                              */
.guide  #4A535C    /* thin vertical guide for grouped operations      */
.focus  #E8796A08 + #E8796A stroke  /* coral focus box, 8% alpha fill */
```

### Current state
Ratatui defaults only. Pure-black background, plain white primary, no coral, no teal, no warm-neutral, no focus box, no soft ivory. Every color choice in the existing panels is wrong against the spec.

### Fix
`M-T14.A`: new `sylvander-tui/src/theme.rs` exporting typed `Style` constants.

---

## 2. Header is inverted (header-on-top vs status-on-top)

### SVG ground truth (`02-tui-immersive.svg` lines 5)
```
Header section:
  line 1 left:  ◖S◗ Sylvander  auth-refactor
  line 1 right: claude-sonnet-5 · plan
  line 2:       ~/Projects/acme-api · feat/auth-refactor · session 8f21
  ───────────  hairline below

Status section (near bottom):
  left:  working · context 24% · 3 tools · main
  right: ↵ send  ⇧↵ newline  esc interrupt  / commands  ? help
```

### Current state
- Top-row is a **status bar** that shows `Sylvander · deepseek-v4-flash · Disconnected · Disconnected: Connection refused`. Three bugs in one line: status on top (wrong region), model name lives in status (wrong region — should be in header right), verbose error string duplicates the meta field.
- No `◖S◗` crab mark anywhere.
- No 2-line identity structure.
- No hairline separator between regions.

### Fix
`M-T14.B`: new `panel/header.rs` that owns identity·session·environment.
`M-T14.C`: relocate `panel/status.rs` to the BOTTOM, in design's `working · context N% · N tools · main` shape, and limit contextual hints to **maximum three** (`18-composer-interactions.svg` rule).

---

## 3. Composer chrome is bare

### SVG ground truth (`18-composer-interactions.svg` IDLE state + FOCUSED state)
- Hairline rule **above** composer
- Bordered panel around the input with rounded corners (we can drop the rounded corners for terminal fidelity, but the borders are required for focus indication)
- Placeholder text `Ask Sylvander…` rendered in `s` class (secondary, dim) when buffer is empty
- Focus state: 3-px coral `│` accent bar on left edge + soft 8%-alpha coral fill behind the input + (when focused) the cursor `│` rendered visibly
- Hairline rule **below** composer
- Helper line **inside** the chrome: `Type while I work — steer, queue, or interrupt.` rendered in `x` class (tertiary, even dimmer)

### Current state
- No borders, no rules, no helper line, no placeholder.
- Composer occupies a single `Length(8)` budget row with `> ` prefix. Looks naked against design.

### Fix
`M-T14.D`: rewrite `panel/input.rs` to wrap input in a bordered Block with hairline rules above and below, add helper line, add focus coral accent via the `focus` style token.

---

## 4. Status row has no contextual meaning

### SVG ground truth (`18-composer-interactions.svg` ADAPTIVE STATUS panel)
The status row carries **the agent's current mode**, not the connection state:
```
Idle            plan · sonnet · context 24%
Working         ◐ working · 18s · 3 tools
Waiting         ● waiting for approval
Disconnected    ! reconnecting · draft preserved
```

### Current state
The status row reports `Disconnected: Connection refused (os error 61)` (the chromium error message doubled with the colored state). Wrong content, wrong position (top-of-screen), wrong styling.

### Fix
`M-T14.C`: replace the status panel content with the design's 4-mode enum (`Idle / Working / Waiting / Disconnected`) driven by `AppMode` plus a transport-state derived mode (`main` / `plan` / `autonomous` etc. — come from server).

---

## 5. Tool rhythm lacks grouping + vertical guide

### SVG ground truth (`02-tui-immersive.svg` lines 9-14)
```
● Exploring the codebase                 00:12
│
✓ Read     src/http/router.rs            126 lines
│
✓ Search   "middleware" in src/         14 matches
│
✓ Read     src/auth/mod.rs              82 lines
│
◐ Inspect  tests/auth_test.rs           running…
```
- Header line: `●` in blue + step name + elapsed on right
- Each sub-task: `✓/◐` glyph + verb in fixed column (`Read/Search/Inspect`) + target + meta
- A **vertical guide line** at indent-x connects header to all sub-tasks (`#4A535C`, 1-px)
- Only one tool rhythm "active" at a time per agent step

### Current state
- Each tool call is its own `ChatMessage::ToolCall` row in the transcript; no grouping.
- No vertical guide, no elapsed time.
- `✓ bash` rendered as `◐ bash` (block duration), `+ bash` for done. Missing `Read`/`Search`/`Inspect` verb column alignment.

### Fix
`M-T14.E`: introduce `ChatMessage::ToolStep { name, started_at, children }` so the reducer can fold consecutive ToolCalls into one grouped step with a vertical guide drawn from canvas primitives, not Block widgets. Add `wire`-driven verb classification (`read`/`bash`/`grep`/`write`…).

---

## 6. Paste attachments are stacked, not tokenized

### SVG ground truth (`18-composer-interactions.svg` LARGE PASTE state)
```
▣ error.log · 84 lines  ×      @file auth/mod.rs  ×
```

- One **token per attachment**, side-by-side
- Each token is a small bordered chip with `▣` (paste) or `@` (file) prefix + name + metric + `×` remove button
- Tokens live **above** the draft rows but **inside** the composer chrome

### Current state
- Attachments stack as `⎘ [paste: 20 lines · 150B] line 1 line 2 …` rows vertically above the input.
- No removable `×` button (would need a key binding like `Backspace` on the focused token).
- No distinction between paste / file in the glyph.

### Fix
Part of `M-T14.D` (composer chrome): render attachments as bordered chips instead of indented rows.

---

## 7. No welcome lockup on first launch

### SVG ground truth (`02-tui-immersive.svg` sketch + `18-composer-interactions.svg` IDLE state)
```
  ◖S◗  SYLVANDER
       intelligent terminal workspace

       ~/Projects/acme-api
       What are we building today?
```
- 4-row lockup on first launch when no messages exist
- After first user message, lockup is dismissed and the conversation surface takes over
- Coral is **only** used for the `◖S◗` glyph; the `SYLVANDER` wordmark is primary text color
- Never inside a conversation; never repeated

### Current state
- Empty TUI shows an empty chat panel and a prompt — which is correct behaviorally but visually cold. The user does not know what to type or what the shell's identity is.

### Fix
`M-T14.F`: `panel/welcome.rs` triggered when `messages.is_empty() && sessions.is_empty()`. Dismissed once a chat is submitted.

---

## 8. Help row is a permanent manual — design explicitly forbids this

### Citation
- Markdown `§2.3` (Claude Code / Gemini synthesis table) lists "Dense permanent footer instructions" as anti-pattern.
- `18-composer-interactions.svg` adaptive-status rule: "Right-side hints are contextual, maximum three. No permanent shortcut manual in the footer."

### Current state
The bottom row permanently reads:
```
Enter:send  Shift+Enter:newline  Esc:quit  Ctrl+C:quit  Ctrl+P:sessions  /:command
```
Six shortcuts, fixed, all the time. Exactly the anti-pattern.

### Fix
`M-T14.C`: replace with **mode-aware contextual hints**, max three, unicode symbols per design (`↵ send`, `⇧↵ newline`, `esc interrupt`, `/ commands`, `? help`).

---

## Plan summary

| Sub-task | Scope | Source |
|---|---|---|
| M-T14.A | palette + theme.rs | §2.1 |
| M-T14.B | header panel (2-line, hairline, ◖S◗) | §5.1 |
| M-T14.C | status row at bottom + contextual hints (≤3) | §5.1 + §18 adaptive-status |
| M-T14.D | composer chrome (borders, placeholders, focus accent, attachment tokens) | §18 IDLE/FOCUSED/PASTE |
| M-T14.E | tool rhythm grouping + vertical guide + verb column | §6 tool rhythm |
| M-T14.F | welcome lockup on empty session | §2.2 + §18 IDLE |

After M-T14:
- Re-snapshot at 120×36 (design's reference viewport from `§5`).
- The result is the truth-of-record for whether the UI matches design.
