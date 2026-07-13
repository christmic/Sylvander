# sylvander-tui

Terminal-native client for the Sylvander Agent. This crate owns terminal input,
application interaction state, the Unix Agent service adapter, and Ratatui
presentation. It does not own the Agent loop, Token9, or Ghostty window UI.

## Module documentation

- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) — layers, dependency rules,
  state ownership, and extension points.
- [`docs/CONFIGURATION.md`](docs/CONFIGURATION.md) — runtime settings and themes.
- [`docs/INPUT-RENDERING.md`](docs/INPUT-RENDERING.md) — keyboard/mouse ownership,
  event loop, frame pacing, scrolling, and rendering performance.
- [`docs/SECURITY.md`](docs/SECURITY.md) — trust boundaries, verified controls,
  redaction coverage, and open release blockers.
- [`docs/INTERACTION-SCENARIOS.md`](docs/INTERACTION-SCENARIOS.md) — scenario-by-scenario
  command, tool, approval, question, session, and recovery behavior.
- [`docs/PRODUCTION-READINESS.md`](docs/PRODUCTION-READINESS.md) — prioritized
  production gap checklist and end-to-end acceptance gates.
- [`../docs/sylvander-tui-ux-design.md`](../docs/sylvander-tui-ux-design.md) —
  normative visual and interaction specification.

## Run

```bash
source sylvander.env
cargo run -p sylvander-tui --locked -- /tmp/sylvander.sock
```

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

The test suite includes a real pseudo-terminal process test. It starts the
compiled binary, negotiates the Unix protocol, submits keyboard input, renders
a streamed reply, rejects approval with a typed reason, answers AskUser,
interrupts an active turn, resizes the terminal, and verifies clean idle exit.
