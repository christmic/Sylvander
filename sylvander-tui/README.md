# sylvander-tui

Terminal-native client for the Sylvander Agent. This crate owns terminal input,
application interaction state, the Unix Agent service adapter, and Ratatui
presentation. It does not own the Agent loop, Token9, or Ghostty window UI.

The transcript follows Claude Code's familiar visual grammar: unframed `❯`
user turns, `⏺` Agent/activity leads, `⎿` child tools, and one ruled live
Composer. Sylvander's Seed-Crab, semantic palette, status model, protocol, and
runtime remain product-owned.

## Module documentation

- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) — layers, dependency rules,
  state ownership, and extension points.
- [`docs/CONFIGURATION.md`](docs/CONFIGURATION.md) — runtime settings and themes.
- [`docs/INPUT-RENDERING.md`](docs/INPUT-RENDERING.md) — keyboard/mouse ownership,
  event loop, frame pacing, scrolling, and rendering performance.
- [`docs/SECURITY.md`](docs/SECURITY.md) — trust boundaries, verified controls,
  redaction coverage, and deployment-specific credential gates.
- [`docs/INTERACTION-SCENARIOS.md`](docs/INTERACTION-SCENARIOS.md) — scenario-by-scenario
  command, tool, approval, question, session, and recovery behavior.
- [`docs/PRODUCTION-READINESS.md`](docs/PRODUCTION-READINESS.md) — implemented
  capability ledger and end-to-end acceptance gates.
- [`../docs/sylvander-tui-ux-design.md`](../docs/sylvander-tui-ux-design.md) —
  normative visual and interaction specification.

## Run

```bash
source sylvander.env
cargo run -p sylvander-tui --locked -- /tmp/sylvander.sock
```

A desktop host can bind one TUI process to one durable session without changing
the standalone interaction model:

```bash
cargo run -p sylvander-tui --locked -- \
  --socket /tmp/sylvander.sock \
  --session session-id \
  --workspace /path/to/workspace
```

The equivalent environment variables are `SYLVANDER_SOCKET`,
`SYLVANDER_SESSION`, and `SYLVANDER_WORKSPACE`. Command-line values win over
environment values. Omitting `--session` preserves the Welcome and session
picker flow.

When Ghostty supplies a session-scoped host capability, the bound TUI also
exposes `/preview image <workspace-path>` and `/preview web <https-url>`. The
host socket and random capability token are injected by Ghostty, are not shown
in `/config`, and must not be configured manually for ordinary standalone use.

Select another built-in theme:

```bash
SYLVANDER_TUI_THEME=midnight cargo run -p sylvander-tui --locked
SYLVANDER_TUI_THEME=high-contrast cargo run -p sylvander-tui --locked
```

## Verification

```bash
cargo check -p sylvander-tui --all-targets --locked
cargo test -p sylvander-tui --locked
```

The test suite includes compiled-binary pseudo-terminal process tests. One
negotiates the Unix protocol, submits keyboard input, renders a streamed reply,
rejects approval with a typed reason, answers AskUser, interrupts an active
turn, resizes the terminal, and verifies reconnect plus clean idle exit. A
second runs the real `AgentRun`, `UnixChannel`, and file-backed SQLite stack
against a locally controlled model endpoint. Its scenarios answer an
Agent-owned AskUser prompt, interrupt a delayed turn, reject a write and verify
it never executes, then start a fresh TUI process and restore the persisted
transcript through `Ctrl+P`. The real-runtime gate also runs two compiled TUI
clients concurrently with a deliberately colliding AskUser ID, verifies scoped
answers and interrupts, reconnects to a live buffered turn, and checks the
file-backed transcripts for cross-client contamination.
