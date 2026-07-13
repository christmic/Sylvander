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

Conversation rendering follows the Claude-familiar hierarchy verified against
Claude Code 2.1.197: submitted user turns start with unframed `❯`, Agent and
primary activity rows start with `⏺`, child tools start with `⎿`, and only the
live bottom Composer is enclosed by full-width rules and owns a hardware cursor.

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
⏺ Run cargo test                         03s
  ⎿  $ cargo test -p sylvander-tui      130 passed
```

Edit and Write show a bounded unified diff immediately after completion, with
removed and added rows using semantic danger/verified colors. `Ctrl+O` or
`/tools expand` reveals the remaining structured input and up to 12 output rows.
Other routine tools stay collapsed. The formatter understands Bash, Read, Write,
Edit, Search, AskUser, and memory tools; unknown tools fall back to key/value
rendering. Error output uses the warning role and remains expandable.

## Command Line

`/` and `Ctrl+K` open the Focus Picker between the Composer and bottom status
row. Its command rows and query share the Composer's left baseline; it has no
redundant title or popup indentation. Commands may be selected or typed with
arguments. Matching accepts contiguous text and ordered
fuzzy characters across names, aliases, and descriptions. `Tab` completes the
selected canonical name;
successful commands move to the front when the next empty palette opens.
Unavailable commands stay visible with the exact prerequisite instead of
disappearing. Invalid arguments remain in the command line with an inline error.

Set `SYLVANDER_TUI_REDUCED_MOTION=1` to disable animation ticks and replace
blinking edit cursors with a static reversed cursor. Set
`SYLVANDER_TUI_NO_ITALIC=1` when the terminal or font renders italics poorly;
Markdown emphasis uses underline while helper, thinking, and brand hierarchy
remain distinct through semantic color and dim intensity. `/config` reports the
resolved values.

| Command | Effect |
|---|---|
| `/new` | Clears current session state locally; next prompt creates a session |
| `/sessions` | Refreshes and opens the session browser |
| `/resume` | Opens the persisted session browser |
| `/rename <name>` | Persists a new label for the current session |
| `/fork` | Copies the current session and its message history |
| `/clear` | Clears local transcript but keeps current session identity |
| `/help [commands\|approval\|tools\|vim]` | Opens visible contextual help |
| `/theme <name>` | Switches semantic palette without changing layout |
| `/mcp` | Shows redacted Agent-advertised MCP configuration, auth metadata, and trust state |
| `/skills` | Shows advertised skill source, activation, trust, and reload state |
| `/memory` | Shows server-reported long-term memory availability and capabilities |
| `/tools [expand\|collapse]` | Controls detailed tool rendering |
| `/model [model-id] [effort]` | Opens the server-backed picker or selects an advertised combination for the next turn |
| `/permissions` | Edits workspace filesystem, network, and approval policy for the next turn |
| `/context` | Reports the last provider-confirmed window/cache usage and structural sources |
| `/compact` | Summarizes older context and preserves recent turns for the current idle session |
| `/status` | Appends model, branch, session, iteration, and token usage |
| `/quit` | Saves input history and exits |

Workspace-owned prompt commands may be declared in the Agent TOML and appear in
the same palette with their source shown:

```toml
[[ui_commands]]
id = "workspace.security-review"
name = "security-review"
usage = "/security-review [scope]"
description = "Review a workspace scope for security issues"
hint = "workspace command"
prompt = "Review {{args}} for security issues."
```

Invoking `/security-review src/auth` submits the expanded prompt through the
normal chat/queue path. Built-in or alias collisions, duplicate IDs/names,
invalid metadata, and external or unverified trust are shown as unavailable;
they are never invoked.

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
no branch. Filesystem rollback is a separate operation; the TUI never implies
that conversation rewind reverted code.

`/checkpoint` creates a full conversation branch and keeps the source session
as the return point. `/undo` returns to that structured source session exactly
once. Both surfaces repeat `workspace files unchanged`; loading any unrelated
session clears the stale undo target. These commands are conversation safety
tools, not filesystem rollback.

`/rollback` is that separate filesystem operation. It covers only successful
Agent `Write` and `Edit` calls recorded by the durable workspace journal; shell
commands and user edits are never inferred. The first request is read-only and
returns the exact file list. A Review View shows the scope and a focus-owning
Decision Dock asks for confirmation, then a second request carries the previewed
turn id. The Agent refuses a stale turn id,
an active turn, symlink/`..` escapes, files over the 8 MiB snapshot limit, or
any file changed since the Agent edit. Every file is conflict-checked before
the first restore. A durable recovery marker completes an interrupted rollback
on the next journal operation. Conversation history is never changed.

## Workspace inspection and review

- `/mention` opens the same bounded, fuzzy workspace-file picker as `@`; it
  attaches the selected file to the draft and never sends by itself.
- `/diff` inspects staged, unstaged, and untracked Git changes. `/diff staged` and
  `/diff unstaged` narrow the scope. The read-only query disables external diff
  drivers and Git locks, caps output at 2 MiB, and opens the existing searchable,
  copyable inspector without changing the transcript or repository.
- `/review` loads the same diff, validates it against the active model attachment
  limit, and sends exactly one typed diff attachment with a findings-first review
  request. It is available only while idle and sends nothing when no changes exist.
- Git failures and non-repository workspaces become bounded visible diagnostics;
  they never degrade into an empty or fabricated review.

## Approval

Approval is a focus-owning Decision Dock inserted below the visible Composer and
above the bottom status row. Keys never leak into global shortcuts or the saved
draft, and the Composer's hardware cursor is hidden while the Dock owns focus.

- One request is shown at a time with intent, exact target, consequence, and
  available scope in that order.
- `↑`/`↓` choose a plain-language action and `Enter` confirms it.
- `y` always allows once; `n`/`r` always deny. Number keys activate the visible
  numbered row, whose order is deliberately safe-first for critical actions.
- `s` approves the exact tool-and-arguments request for the current session.
- `p` persists that exact request across sessions only when the server advertises
  persistent approval. It is hidden when the operator has not configured a store.
- Denial opens an optional, bounded guidance input. The
  reason is attached to the typed approval decision; it never becomes a second
  chat turn.
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

## Interaction timeouts

Timeouts are typed Agent events rather than TUI estimates. Approval waits use a
120-second deadline, questions and plan reviews use 300 seconds, registered
tools use their execution-budget deadline (120 seconds by default), and
background investigations stop after 600 seconds.

- Approval and plan timeouts reject the pending decision; question timeout
  resumes with an empty answer. All three remove their Agent-owned pending entry.
- A matching approval, question, or plan surface closes immediately, so a late key
  press cannot submit a decision for work that has already resumed.
- Tool timeouts settle the tool row as an error. Background-task timeouts emit
  both the timeout and the terminal failed state; neither leaves a spinner alive.
- The transcript keeps the timeout kind, bounded subject id, actual deadline,
  and Agent-selected recovery: request again, retry with a narrower scope, or
  continue without the missing result.
- Recovery text is guidance, not an automatic retry. Retrying remains an
  explicit user/Agent turn and never replays a write or shell command silently.

## Sessions

The standalone TUI owns one active session. This browser is a temporary Focus
Picker for loading a different persisted session; it is not a sidebar and does
not imply that several sessions are active at once. Multi-session navigation is
owned only by a Ghostty host running independent TUI processes.

- Opening the browser requests current persisted session metadata from the
  service and merges it with locally observed sessions.
- Typing filters while `↑`/`↓` continue to move selection. `Tab` exposes the
  secondary rename/archive actions without turning the candidates into live
  session tabs.
- `Ctrl+N` prepares a new session without sending an empty Agent message.
- Rename persists through `SessionStore`; delete confirmation archives the same
  original entry even when the list is filtered.
- Switching requests stored history and replaces the one loaded transcript only
  after the service responds; until then the current conversation and draft are
  unchanged. Switching is disabled while a turn is active.
- Fork copies stored message history into a new session id and opens the fork.
- The Agent and Unix channel share one SQLite store. The Agent restores model
  history when joining a known session and persists terminal assistant output
  before publishing Done.

## Connectivity

Before any runtime or session request, the TUI and Agent service negotiate an
overlapping UI protocol version and exchange named capabilities. A timeout,
incompatible range, malformed welcome, or business message before the handshake
keeps the client disconnected with a visible reason. Unknown post-handshake
messages become bounded transcript diagnostics instead of disappearing; their
raw payload is not displayed.

The runtime reconnects to the Unix Agent service on a configurable interval.
When a session is active, reconnect requests a reattachment rather than merely
opening another socket. The service atomically returns durable history followed
by the ordered in-flight turn replay, including text, tool, approval, question,
plan, and task events that arrived while disconnected. Local queued prompts and
drafts survive the replacement. The replay is capped at 4 MiB; overflow remains
bounded and produces an explicit recovery notice instead of silently presenting
an incomplete transcript. A second concurrent turn for the same session is
rejected rather than interleaved.

`/doctor` opens a redacted runtime report, `/doctor copy` sends that same report
through the bounded terminal clipboard, and `/doctor export <path>` writes it
with a same-directory temporary file and atomic rename. Reports contain resolved
TUI/runtime state but only path basenames; environment variables, credentials,
full home paths, prompt contents, and tool output are excluded.

## Backend-dependent Surfaces

Plan review and background-task rendering already have UI states and snapshot
coverage, but their live activation depends on the Agent publishing the matching
public `StreamEvent`. The TUI must not invent a parallel UI event protocol to
simulate them.
