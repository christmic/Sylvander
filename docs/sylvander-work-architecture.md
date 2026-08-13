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

`AskUser` uses the same answer encoding already proven by the TUI: one choice
is sent verbatim, multiple choices use `, `, and optional free text follows
selected choices after `; `. The desktop sends only the public `Answer`
command. It owns temporary form selection and clears that form after submission
or a turn terminal; Runtime owns whether and how execution resumes.

Plan review retains Runtime's `plan_id` alongside display steps. Approve and
reject send the protocol's typed `PlanDecision` and clear only the pending
review control after successful submission; they do not mark execution
complete. Revised steps remain represented by the same protocol decision and
will be enabled with the dedicated editor surface rather than an ad-hoc wire
message.

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

After a transport or handshake failure, the React application coordinator
reopens the native gateway with exponential backoff capped at ten seconds.
Only the Rust gateway repeats authentication and protocol negotiation; the
WebView never receives the bearer. An initial failure remains visibly offline,
while loss of an established link is visibly reconnecting. Component teardown
cancels pending retries and closes the native connection.

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

1. Build the responsive React shell and typed presentation store.
2. Add the bounded native Rust WebSocket gateway and protocol conformance tests.
3. Add approval, AskUser, plan, task, diff, artifact, and settings surfaces.
4. Add native dialogs, notifications, window persistence, signing, and updates.
5. Establish cross-platform performance and accessibility release evidence.
