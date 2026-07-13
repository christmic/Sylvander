# Input, Scrolling, and Rendering

## Input ownership

Keyboard and mouse scrolling are intentionally different, matching the behavior
expected from a coding Agent TUI.

| Input | Owner | Effect |
|---|---|---|
| `↑` / `↓` | Composer | Move within the draft or browse submitted input history |
| `PageUp` / `PageDown` | Transcript | Review older/newer transcript pages |
| `Ctrl+End` | Transcript | Return to live output and clear unread count |
| Mouse wheel up/down | Transcript | Scroll configured row count; never touches Composer history |
| Enter | Composer | Submit |
| Shift+Enter | Composer | Insert newline |
| Bracketed paste | Composer | Inline short text or create an attachment token |
| Resize | Presentation | Mark the frame dirty and recompute layout |
| `Ctrl+O` | Transcript presentation | Toggle structured tool input/result detail |
| `/` / `Ctrl+K` | Command line | Open command selection and argument input |

Decision overlays own keyboard input while open. Mouse-wheel transcript scrolling
does not dismiss an overlay or synthesize selection keys.

Set `SYLVANDER_TUI_EDITING=vim` for a modal Composer. It starts in Insert mode;
`Esc` enters Normal mode while idle, but continues to interrupt active Agent work.
Normal mode supports `h/j/k/l`, arrows, `w/b`, `0/$`, `gg/G`, `i/a/I/A`,
`o/O`, `x`, `D`, `dd/dw/d$`, `cc/cw/c$`, `yy/yw`, `p/P`, `u`, and `Enter`
to submit. Deletes and yanks use a Composer-local register; insert/change
sequences form one undo unit. `/help vim` lists the complete supported grammar.
Approval, question, command-palette, and global safety bindings are unchanged.
The active Vim mode is always visible in the status row.

## Live-follow behavior

- At `chat_scroll == 0`, the transcript follows live output.
- Scrolling upward sets a positive offset and preserves the viewport anchor.
- New events received while detached increment `unread_events` without moving the
  viewport.
- Scrolling down to zero or pressing `Ctrl+End` returns to live output and clears
  unread state.

## Event-driven runtime

The old main loop slept for 200ms after every drain, causing up to 200ms keyboard
latency. The runtime now uses `tokio::select!` with independent wake sources:

```text
terminal input ───────── immediate state update + immediate render
service event ────────── state update, render on next frame clock
frame clock (60 FPS) ─── coalesced streaming render
animation clock (200ms)  low-frequency status/elapsed update
reconnect clock (1500ms) retry a disconnected Agent service
```

`SYLVANDER_TUI_RENDER_FPS` and `SYLVANDER_TUI_ANIMATION_MS` configure these
clocks. Service bursts are capped at 256 events per cycle and input bursts at 64
events so neither source can starve the other. Input collected while handling a
service wake is rendered before the remaining service burst. Draft persistence
is debounced by 250ms and flushed on exit, keeping filesystem writes out of the
keystroke path.

Composer editing uses Unicode grapheme boundaries and terminal cell width rather
than Unicode scalar count. Wide CJK glyphs therefore occupy two cells, combining
sequences move and delete as one visible unit, and an eight-row draft window
keeps the logical cursor visible. The terminal owns IME pre-edit and candidate
UI; committed text and the hardware cursor remain aligned inside the TUI. When
the Composer owns focus, even an empty draft exposes the hardware cursor after
the `> ` prompt. A temporary interaction surface takes that cursor while open.

The queues themselves are bounded independently of those per-cycle drain limits:
service events apply socket backpressure at 1024 items; terminal input retains at
most 256 intents, dropping only redundant scroll/resize events when saturated.
Keyboard and paste input are never silently dropped. The local transcript view is
bounded to 2000 entries and 16 MiB, with UTF-8-safe per-message and tool-payload
limits. Pruning leaves an explicit transcript notice; persisted session history
remains Agent-owned and can be loaded again with `/resume`.

Composer retention is bounded separately: 256 KiB/1024 rows for draft text,
32 attachments, and 2 MiB per local attachment before the active model's
usually smaller advertised limit is applied. Oversized paste is rejected with a
visible status; external-editor text is UTF-8-safely truncated with an explicit
notice. Draft restore validates the same limits before retaining content.
Prompts submitted during active work use a 100-item FIFO; once full, Enter keeps
the draft intact and directs the user to remove an item before queuing more.
The session selector caches the 5000 most recent summaries while the Agent's
persistent store remains authoritative.
Decision overlays have a 64-layer hard ceiling. If a malformed or overloaded
service exceeds it, Approval, AskUser, and Plan requests receive typed terminal
decisions immediately so no Agent waiter is abandoned; the overflow is also
visible in the transcript.

## Dirty rendering

Rendering occurs only when state is dirty. Idle animation ticks do not dirty a
still interface. User input bypasses the frame clock for immediate feedback;
streaming deltas are coalesced to avoid cursor churn and excess terminal writes.

Panels are pure renderers:

- Input: `&AppState`, `Rect`, active semantic Palette.
- Output: terminal cells in the provided `Frame`.
- Forbidden: state mutation, socket calls, environment reads, filesystem reads,
  subprocess execution.

## Regression tests

At minimum, preserve tests for:

- keyboard Up recalls Composer history without moving transcript;
- mouse wheel changes transcript without changing Composer history;
- mouse wheel down returns to live and clears unread;
- idle ticks do not schedule repaint;
- streaming and settled replies keep the same vertical origin;
- wide/fullscreen resize keeps the transcript left anchored;
- Composer wrapping grows upward without moving the status row;
- CJK, combining sequences, and exact-width wrapping preserve the hardware
  cursor's terminal-cell position;
- redraw floods remain bounded and do not drop a subsequent keyboard event;
- transcript count/byte budgets and streaming/tool payload limits remain bounded.
- the compiled binary completes chat, streamed rendering, approval rejection,
  AskUser, interrupt, resize, and idle exit inside a real pseudo-terminal rather
  than only a `TestBackend`; the same process also reconnects and reapplies typed
  session history before continuing work.
