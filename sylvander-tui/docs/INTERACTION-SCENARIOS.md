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

While a turn is active, `Enter` adds the prompt to a local FIFO instead of
opening a concurrent subscription for the same session. `/queue` lists pending
prompts; `/queue edit <n> <text>`, `/queue drop <n>`, and `/queue clear` mutate
the queue. A terminal Done, Error, or Interrupted event starts exactly one next
prompt.

`Esc` or `Ctrl+C` during active work sends a session-scoped interrupt. It does
not quit the TUI or send the Agent-wide Stop command. The terminal interrupted
event preserves partial prose, settles pending tool rows, closes decision
surfaces, and then advances the local queue.

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
| `/resume` | Opens the persisted session browser |
| `/rename <name>` | Persists a new label for the current session |
| `/fork` | Copies the current session and its message history |
| `/clear` | Clears local transcript but keeps current session identity |
| `/help [commands\|approval\|tools]` | Opens visible contextual help |
| `/theme <name>` | Switches semantic palette without changing layout |
| `/tools [expand\|collapse]` | Controls detailed tool rendering |
| `/model [model-id] [effort]` | Opens the server-backed picker or selects an advertised combination for the next turn |
| `/permissions` | Edits workspace filesystem, network, and approval policy for the next turn |
| `/context` | Reports the last provider-confirmed window/cache usage and structural sources |
| `/compact` | Summarizes older context and preserves recent turns for the current idle session |
| `/status` | Appends model, branch, session, iteration, and token usage |
| `/quit` | Saves input history and exits |

The model picker never carries a hard-coded catalog. `↑`/`↓` chooses among
server-advertised models and `←`/`→` chooses only reasoning efforts supported by
the selected model. `Enter` sends one typed selection request. The Agent validates
the pair and the Unix service returns updated runtime truth; status and Welcome
change only after that acknowledgement. A selection made during active work is
applied to the next turn because every turn owns an immutable model snapshot.

The permissions picker is likewise server-backed and turn-scoped. Filesystem
access is `none`, `read only`, or `workspace write`; the root remains the active
session workspace and cannot be replaced by the TUI. Network is denied or
allowed through `ToolContext`. Approval is ask, allow, or deny; ask is omitted
when the server operator did not enable approval prompts. The Agent constructs a
fresh tool context from the acknowledged profile at the start of every turn.

`/context` requests a fresh report from the Agent. Window occupancy uses the
last provider `Usage` for that session, including cache-read and cache-creation
input; it is intentionally separate from the session's cumulative billing
counters. Source rows count verifiable structures (system instructions,
conversation messages, and tool definitions). Sylvander does not invent
per-source token estimates when the provider did not return them.

`/compact` is a server-backed operation, not a local transcript clear. It is
rejected while the session has active work. The TUI shows start, completion,
and failure events; completion includes removed messages, condensed blocks,
estimated freed tokens, and the bounded resulting summary. Automatic semantic
compaction emits the same lifecycle. The Agent replaces its live history and,
when a session store exists, atomically changes the active durable history to
the summary followed by preserved recent messages so restart does not resurrect
the pre-compaction context.

Provider retries are typed before reaching presentation. Rate limits, provider
5xx failures, network failures, and interrupted response streams have distinct
labels while retaining attempt count, maximum attempts, backoff duration, and a
bounded diagnostic reason. Older servers without a cause field safely render a
generic model retry.

Model lifecycle is also server truth. `/model` marks deprecated catalog rows
and shows an advertised replacement when one exists. If the active model is
deprecated, the transcript and status surface `old → replacement`; selection
remains available so persisted sessions are not broken silently.

Session cost is calculated by the Agent from the model pricing snapshot used
for each iteration, accumulated durably, and restored with session history.
The wide status bar shows the estimated USD amount and `/status` expands it.
When pricing is absent, historical usage predates pricing, or non-zero cache
usage lacks a cache rate, the UI says `cost unavailable` instead of displaying
a misleading zero or partial estimate.

`/rewind <completed-turn>` is conversation-only and non-destructive. The
service validates an assistant-completed boundary, creates a new persisted
session containing history through that boundary, and leaves both the source
session and workspace files unchanged. Invalid or unfinished boundaries create
no branch. Filesystem rollback is intentionally a separate future operation;
the TUI never implies that conversation rewind reverted code.

`/checkpoint` creates a full conversation branch and keeps the source session
as the return point. `/undo` returns to that structured source session exactly
once. Both surfaces repeat `workspace files unchanged`; loading any unrelated
session clears the stale undo target. These commands are conversation safety
tools, not filesystem rollback.

## Approval

Approval is a focus-owning decision layer. Keys never leak into global shortcuts
or the Composer.

- Each request shows risk, semantic action, and filesystem/process scope.
- `Enter`, `y`, or `1` approves the selected request once.
- `s` approves the exact tool-and-arguments request for the current session.
- `p` persists that exact request across sessions only when the server advertises
  persistent approval. It is hidden when the operator has not configured a store.
- `n`, `r`, or `2` rejects it and opens optional feedback input.
- `a`/`Y` approves all remaining requests once; `N` rejects all remaining requests.
- `Esc` and `Ctrl+C` reject every pending request before closing. The Agent is
  never left waiting on an abandoned approval modal.
- Completion appends a compact approved/rejected summary to the transcript.

Approval lifetime is Agent-owned rather than a local TUI preference. The public
event carries the allowed scopes, transports forward the selected scope without
interpreting it, and the Agent rejects a forged or unavailable scope. Session
grants are isolated by session ID. Persistent grants are written atomically and
match the normalized tool name plus complete JSON arguments; changing a path,
command, or content requires a new decision.

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

- Opening the browser requests current persisted session metadata from the
  service and merges it with locally observed sessions.
- Filtering and selection have separate focus; `Tab` switches focus.
- `Ctrl+N` prepares a new session without sending an empty Agent message.
- Rename persists through `SessionStore`; delete confirmation archives the same
  original entry even when the list is filtered.
- Switching requests stored history and replaces the transcript only after the
  service responds. It is disabled while a turn is active.
- Fork copies stored message history into a new session id and opens the fork.
- The Agent and Unix channel share one SQLite store. The Agent restores model
  history when joining a known session and persists terminal assistant output
  before publishing Done.

## Connectivity

The runtime reconnects to the Unix Agent service on a configurable interval.
Draft and input history survive disconnection. Service events are coalesced;
keyboard feedback remains immediate.

## Backend-dependent Surfaces

Plan review and background-task rendering already have UI states and snapshot
coverage, but their live activation depends on the Agent publishing the matching
public `StreamEvent`. The TUI must not invent a parallel UI event protocol to
simulate them.
