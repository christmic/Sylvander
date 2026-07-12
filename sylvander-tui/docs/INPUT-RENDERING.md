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
frame clock (30 FPS) ─── coalesced streaming render
animation clock (200ms)  low-frequency status/elapsed update
reconnect clock (1500ms) retry a disconnected Agent service
```

`SYLVANDER_TUI_RENDER_FPS` and `SYLVANDER_TUI_ANIMATION_MS` configure these
clocks. Service bursts are capped at 256 events per cycle and input bursts at 64
events so neither source can starve the other.

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
- Composer wrapping grows upward without moving the status row.
