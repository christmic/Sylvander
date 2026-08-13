# Sylvander Work architecture

> Status: accepted foundation decision
>
> Updated: 2026-08-13

## Decision

Sylvander Work uses a Tauri 2 native shell, a React 19 TypeScript presentation
layer, and a Rust-owned gateway for the existing authenticated
JSON-over-WebSocket UI protocol.

Sylvander Work is a presentation client. Runtime remains the sole
owner of authenticated Sessions, Agent execution, provider credentials,
authorization, storage, tools, and observability.

```text
React presentation and interaction state
                |
          typed Tauri Channel
                |
        bounded Rust gateway
                |
  sylvander-channel-ws / public UI protocol
                |
       Runtime ChannelHost boundary
```

## Evidence

### Existing Sylvander boundaries

| Evidence | Confirmed behavior |
|---|---|
| `docs/product-module-architecture.md` | Presentation clients depend on the service edge and public API; Runtime is the application composition root. |
| `sylvander-api/src/ui.rs` | The public contract already covers protocol negotiation, Sessions, history, streaming, tools, approvals, questions, plans, tasks, runtime information, context, and coding-session review. |
| `sylvander-channel-ws/src/lib.rs` | The WebSocket adapter already provides authenticated, bounded, full-duplex JSON transport for desktop clients. |
| `sylvander-tui/src/client.rs` | The TUI proves that a thin client can negotiate the protocol and render Runtime-owned Session state without embedding Runtime. |
| `sylvander-runtime/docs/channel-supervision.md` | A separate client may provide multi-session presentation without changing Channel lifecycle ownership. |

### Current upstream evidence

The following upstream sources were inspected on 2026-08-13:

- Tauri's official overview documents a Rust shell that uses the operating
  system WebView and can produce a minimal application below 600 KiB:
  <https://v2.tauri.app/start/>.
- Tauri's official architecture identifies TAO for windows and WRY for WebView
  integration: <https://github.com/tauri-apps/tauri/blob/dev/ARCHITECTURE.md>.
- Tauri's official capability system denies potentially dangerous plugin
  commands unless explicitly granted:
  <https://v2.tauri.app/security/capabilities/>.
- Tauri recommends channels for streaming Rust data to a frontend:
  <https://v2.tauri.app/develop/calling-rust/>.
- React's official release line documents React 19.2 as the current stable
  feature release: <https://react.dev/blog/2025/10/01/react-19-2>.
- Vite 8 uses the Rust-based Rolldown bundler and its official release pairs
  the React integration with `@vitejs/plugin-react` v6:
  <https://vite.dev/blog/announcing-vite8>.
- Kun uses one runtime for GUI and TUI and makes plans, approvals, background
  tasks, diffs, and verification visible in one workbench:
  <https://github.com/KunAgent/Kun>.
- ModelStudio OpenWork separates desktop Session/workspace/permission
  presentation from its Agent runtime:
  <https://github.com/modelstudioai/openwork>.
- Different AI OpenWork similarly keeps filesystem mutations and Agent
  behavior server-owned while Tauri owns native shell concerns:
  <https://github.com/different-ai/openwork/blob/dev/ARCHITECTURE.md>.

These products are interaction and architecture evidence only. Their code is
not copied, and Kun's non-commercial license is not introduced into Sylvander.

## Why this stack

| Candidate | Strength | Decision |
|---|---|---|
| Tauri 2 + React 19 | Small system-WebView shell, Rust-native host boundary, durable ecosystem, mature accessibility/testing support, and a stable component model | Selected |
| Tauri 2 + Svelte 5 | Smaller presentation runtime and concise reactivity | Rejected for this product: React's ecosystem and long-term staffing/maintenance advantages outweigh the modest view-layer cost. |
| Electron + React | Broad ecosystem and competitor precedent | Rejected for the foundation because bundling Chromium and Node conflicts with the resource-efficiency goal. |
| Slint | Native compiled Rust UI and lightweight rendering | Deferred: rich Markdown, diff, browser preview, accessibility, and desktop component coverage would add product risk. |
| GPUI / egui / iced | Direct Rust rendering and strong control | Deferred: the first release values complete desktop interaction and assistive semantics over owning a renderer. |

Tauri does not make Web content native widgets. Platform WebViews can render
differently, so the desktop client owns explicit typography, focus, contrast,
motion, and screenshot acceptance tests.

## Ownership rules

The React layer owns only ephemeral presentation state:

- selected navigation surface and selected Session;
- Composer drafts and local focus;
- expanded transcript rows and inspector tabs;
- connection presentation and retry affordances;
- optimistic visual acknowledgement that never claims durable completion.

Runtime and the public protocol own all durable or authoritative state. The
desktop client must not open Runtime databases, execute tools, read arbitrary
workspace paths, discover credentials, or infer success from a closed stream.

Long-lived protocol objects are projected by their Runtime identity. For
example, task rows are keyed by `task_id` and started/progress/terminal events
update that row idempotently, so reconnect replay cannot duplicate work. The
projection may retain display text and status while the process is alive, but
it is not a second task store and never synthesizes a terminal state.

Approval batches follow the same rule. The desktop retains every Runtime
`call_id`, presents one least-authorizing decision at a time, and removes an
item only after Runtime publishes its tool start/result or rejection. A turn
terminal clears any remaining presentation prompt; the client does not infer
execution from a button click.

Approval actions render exactly the Runtime-provided `allowed_scopes`. Missing
scope metadata falls back to the protocol default `Once`; Desktop never adds a
Session or Persistent authorization on its own. The selected scope is forwarded
unchanged and still waits for Runtime's subsequent tool fact.

`AskUser` uses the same answer encoding already proven by the TUI: one choice
is sent verbatim, multiple choices use `, `, and optional free text follows
selected choices after `; `. The desktop sends only the public `Answer`
command. It owns temporary form selection and clears that form after submission
or a turn terminal; Runtime owns whether and how execution resumes.

Plan review retains Runtime's `plan_id` alongside display steps. Approve and
reject send the protocol's typed `PlanDecision` and clear only the pending
review control after successful submission; they do not mark execution
complete. Revised steps remain represented by the same protocol decision and
are edited in a dedicated, Session-local form. Empty rows are removed at the
presentation boundary and an all-empty revision is rejected locally; accepted
rows travel in `PlanDecision::Revised` without an ad-hoc wire message.

Running task rows may issue the public `CancelTask` command with Runtime's
`session_id` and `task_id`. Submission does not optimistically mark the row;
only the subsequent `TaskCancelled` event changes its state and records the
authoritative reason. Terminal tasks expose no cancellation control.

Session creation starts with Runtime `DiscoverAgents`; the form submits a
public `SessionCreateRequest` using the selected Agent identity and no invented
defaults. Desktop never allocates a Session identifier or inserts an optimistic
Session row. It selects and loads the Session only after `SessionCreated`, then
refreshes Runtime's authoritative Session list.

Rename, archive, and permanent delete send only their public Session commands.
The desktop does not mutate its Session list on button submission: label and
membership change only on `SessionUpdated` or `SessionDeleted`. Removing the
selected Session also clears its ephemeral transcript, plan, and task
projection so stale work is never displayed under an empty selection.

The header and Composer render the negotiated server name/version and
Runtime-projected provider-qualified model, reasoning effort, and permission
profile. No protocol version, model, or capability is inferred from a model
name or hard-coded in the view; missing information renders an explicit
placeholder until `Welcome` or `RuntimeInfo` arrives.

Before the admitted-turn protocol chain is integrated into this branch, chat
submission uses a deliberately local `waiting` lock to prevent duplicate sends.
The first Runtime activity promotes it to `active`; only `Done`, `Error`, or
`TurnInterrupted` releases it, while a failed native submission rolls it back.
This is a concurrency guard, not an authoritative turn-start claim. The newer
Runtime `TurnStarted` fact remains the required integration authority.

Retry and timeout events are rendered from their typed cause, kind, duration,
and recovery fields; Desktop does not classify reason text. A timeout dismisses
only the matching approval, question, or plan control. Public operation and
boundary errors become failed notices using the server-safe message, with
bounded retry timing when supplied; credentials and resource internals have no
protocol field to render.

The Tauri shell is restricted to window lifecycle, bounded Runtime transport,
native dialogs, notifications, and future signed updates. Every capability is
deny-by-default and scoped to the main window. Shell commands and filesystem
plugins are not enabled in the foundation.

## Transport contract

The native Rust gateway connects to a configured `ws://` or `wss://` endpoint,
sends `UiClientMessage::Hello`, and waits for `UiServerMessage::Welcome` before
submitting work. The bearer lease is supplied to the native transport and is
never passed to JavaScript, persisted in browser storage, or rendered in
diagnostics. The WebView content security policy does not allow arbitrary
network connections, so React cannot bypass the native gateway.

Inbound messages are bounded by the server's configured WebSocket limit.
Streaming deltas are coalesced to at most one presentation update per animation
frame. Terminal `Done`, `Error`, or `TurnInterrupted` events settle a turn;
disconnect never implies completion.

Assistant text, thinking, and tool-output deltas share one ordered per-Session
animation-frame queue. Adjacent events for the same target are merged, while
target changes preserve order. Replayed deltas for a non-selected Session are
dropped from presentation state; authoritative history remains reloadable from
Runtime. A final tool result replaces its streamed preview.

A turn terminal flushes that queue and freezes presentation-only streaming
identities so the next turn cannot append to the previous answer. `Done` uses
Runtime's final text as authoritative, while `Error` and `TurnInterrupted`
retain partial output and add their public reason. Any still-running tool row
is settled as failed because no later event belongs to that terminal turn.

After a transport or handshake failure, the React application coordinator
reopens the native gateway with exponential backoff capped at ten seconds.
Only the Rust gateway repeats authentication and protocol negotiation; the
WebView never receives the bearer. An initial failure remains visibly offline,
while loss of an established link is visibly reconnecting. Component teardown
cancels pending retries and closes the native connection.

Each native connection owns a monotonic Rust-side generation. A socket task may
clear gateway state and publish `Disconnected` only while its generation is
still current. Replaced or explicitly closed tasks therefore cannot race a new
handshake and schedule a redundant WebView reconnect.

Handshake rejection preserves the protocol's public error code, bounded safe
message, and supported version range. Native transport, TLS, and credential
errors remain generic. This gives the user an actionable compatibility fact
without exposing a bearer, request header, or socket diagnostic.

After a successful reconnect, Desktop sends `ReattachSession` for the selected
Runtime identity instead of treating recovery as an ordinary history load.
`SessionHistory.recovery` marks that response, `notice` is rendered verbatim as
a public notice, and `replay_truncated` marks it failed-visible before the Unix/
WebSocket relay resumes buffered in-flight events. Initial selection continues
to use `LoadSession`.

Session history also projects Runtime's iteration count, input/output tokens,
optional nano-USD cost, and source Session identity. These are read-only run
artifacts: the inspector formats them for display, clears them on selection
change/removal, and never recomputes provider usage or price locally.
`IterationStart` activates the Session, while each `IterationEnd` advances the
completed iteration count and replaces token/cost fields with Runtime's
persisted cumulative values.

An active or locally admitted turn replaces Send with Stop. Stop emits the
public `Interrupt` command exactly once and remains pending until Runtime emits
a terminal event. Desktop never treats command delivery as proof that execution
has stopped.

The Context inspector requests provider-confirmed occupancy from Runtime and
renders its structural sources and cache counters. Manual or automatic
compaction remains a Runtime operation: Desktop disables duplicate requests
while `CompactionStarted` is live and reports only the completed or failed
public result.

Model and permission selection is catalog-driven. Desktop keeps provider and
model identifiers paired, offers only each descriptor's advertised reasoning
efforts, and omits `ask` when Runtime disables approvals. Submissions are typed
requests; displayed settings change only after a later `RuntimeInfo` fact.
Session-specific settings additionally load the Runtime revision and per-field
provenance. Pinning or restoring inheritance uses explicit `set`/`inherit`
patches against that revision, so omitted fields—including the redacted,
write-only system prompt—remain untouched. A later `SessionConfig` is the only
source of the next revision and displayed provenance.
The settings surface also exposes an explicit liveness round trip. `Ping`
enters a checking state and only `Pong` establishes healthy; connection state
continues to come from the native socket lifecycle.

The Changes inspector never reads Git directly. It requests Runtime's coding
Session diff, keeps failure alongside the still-reviewable patch, and waits for
accepted/discarded facts. Discard removes the Session projection because the
Runtime operation deletes the isolated worktree and durable Session.

Workspace rollback is a two-phase Runtime operation. Desktop cannot show a
confirmation before `WorkspaceRollbackPreview`, and confirmation echoes the
preview's opaque `turn_id` as `expected_turn_id`. Completion reports restored
files while explicitly leaving conversation history unchanged; failure never
claims a local mutation.

Production builds contain no fixture or demo gateway. Unit and component tests
may inject an in-memory implementation of the gateway interface; it is never
included in the application bootstrap. The compiled
`sylvander-channel-ws` journey remains the protocol acceptance boundary.

## Version baseline

The foundation pins exact, verified stable patches so a clean build is
reproducible: Tauri `2.11.5`, React and React DOM `19.2.8`, Vite `8.2.1`,
TypeScript `6.0.3`, and Vitest `4.1.10`. Dependency updates are deliberate
maintenance changes with build, protocol, visual, and accessibility evidence;
"latest" is not resolved dynamically during release builds.

## Performance and quality gates

These are acceptance targets, not current claims:

- input is painted within one 16.7 ms frame under a normal streaming load;
- stream updates trigger at most one transcript render per animation frame;
- Session navigation remains responsive with 500 Sessions through windowing;
- transcript detail is progressively disclosed and long tool output is not
  mounted until expanded;
- the release JS/CSS bundle and installer size are recorded in CI;
- idle and active RSS, first-window time, and reconnect latency are recorded on
  macOS, Windows, and Linux release runners;
- keyboard-only operation, focus visibility, reduced motion, 200% zoom, and
  automated accessibility checks pass before release.

## Delivery slices

The message-by-message implementation ledger is
[`sylvander-work-protocol-coverage.md`](sylvander-work-protocol-coverage.md).
It is the acceptance SSOT for the service edge; this architecture document
defines ownership and invariants.

1. Build the responsive React shell and typed presentation store.
2. Add the bounded native Rust WebSocket gateway and protocol conformance tests.
3. Add approval, AskUser, plan, task, diff, artifact, and settings surfaces.
4. Add native dialogs, notifications, window persistence, signing, and updates.
5. Establish cross-platform performance and accessibility release evidence.
