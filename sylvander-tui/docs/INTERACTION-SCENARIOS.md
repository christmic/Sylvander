# TUI Interaction Scenarios

This is the implementation contract for interactive TUI behavior. A scenario is
complete only when input ownership, state transition, service effect, rendering,
and recovery behavior are all defined and tested.

## Conversation and Composer

| Scenario | Interaction | Render contract |
|---|---|---|
| Submit | `Enter` | User turn appends immediately; response streams below it |
| Newline | `Shift+Enter` | Composer grows upward; status remains fixed |
| Input history | `↑` / `↓` | Recalls submitted prompts; never scrolls transcript |
| Transcript history | wheel or `PageUp/PageDown` | Detaches from live output without changing Composer |
| Return live | `Ctrl+End` | Clears unread count and follows streaming output |
| Paste | bracketed paste | Short text is inline; large text becomes an attachment token |

## Tool Activity

Tool calls are paired by `call_id`, never by display name. This is required when
the Agent starts multiple calls to the same tool.

Compact mode shows one semantic row per call:

```text
✓ Run cargo test                         03s
│ ✓ $ cargo test -p sylvander-tui       130 passed
```

`Ctrl+O` or `/tools expand` reveals structured input and up to 12 output rows.
The formatter understands Bash, Read, Write, Edit, Search, AskUser, and memory
tools; unknown tools fall back to key/value rendering. Error output uses the
warning role and remains expandable.

## Command Line

`/` and `Ctrl+K` open the command line. Commands may be selected or typed with
arguments. Invalid arguments remain in the command line with an inline error.

| Command | Effect |
|---|---|
| `/new` | Clears current session state locally; next prompt creates a session |
| `/sessions` | Refreshes and opens the session browser |
| `/clear` | Clears local transcript but keeps current session identity |
| `/help [commands\|approval\|tools]` | Opens visible contextual help |
| `/theme <name>` | Switches semantic palette without changing layout |
| `/tools [expand\|collapse]` | Controls detailed tool rendering |
| `/status` | Appends model, branch, session, iteration, and token usage |
| `/quit` | Saves input history and exits |

## Approval

Approval is a focus-owning decision layer. Keys never leak into global shortcuts
or the Composer.

- Each request shows risk, semantic action, and filesystem/process scope.
- `Enter`, `y`, or `1` approves the selected request.
- `n`, `r`, or `2` rejects it and opens optional feedback input.
- `a`/`Y` approves all remaining requests; `N` rejects all remaining requests.
- `Esc` and `Ctrl+C` reject every pending request before closing. The Agent is
  never left waiting on an abandoned approval modal.
- Completion appends a compact approved/rejected summary to the transcript.

Risk labels are explanatory, not policy decisions: Read/Search are low, writes
are medium, shell execution is high, and destructive shell patterns are
critical. The server remains the authority that decides whether approval is
required.

## Agent Questions

- Arrow keys move through options; number keys select directly.
- Space toggles multi-select options.
- Free text can supplement or replace predefined choices.
- Empty submission stays open with validation feedback.
- `Esc` sends an explicit empty answer so the waiting Agent resumes instead of
  timing out.

## Sessions

- Opening the browser requests current session metadata from the service and
  merges it with locally observed sessions.
- Filtering and selection have separate focus; `Tab` switches focus.
- `Ctrl+N` prepares a new session without sending an empty Agent message.
- Rename is an inline local label edit. Delete confirmation removes the same
  original entry even when the list is filtered.
- Switching clears the previous transcript to prevent cross-session content
  from appearing under the newly selected session identity.

## Connectivity

The runtime reconnects to the Unix Agent service on a configurable interval.
Draft and input history survive disconnection. Service events are coalesced;
keyboard feedback remains immediate.

## Backend-dependent Surfaces

Plan review and background-task rendering already have UI states and snapshot
coverage, but their live activation depends on the Agent publishing the matching
public `StreamEvent`. The TUI must not invent a parallel UI event protocol to
simulate them.
