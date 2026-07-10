# Agent Development Guide

This file guides [coding agents](https://agents.md/) working on the
**Sylvander** repository. It is the project's **top-level** guide;
for specific subdirectories, see the **sub-directory `AGENTS.md`
files** which **override** this one when they exist (deepest match
wins).

Sub-directory guides:

- `sylvander-ghostty/AGENTS.md` — shared Zig core (the bulk of the
  terminal emulator) and the GTK (Linux/FreeBSD) app.
- `sylvander-ghostty/CLAUDE.md` — same content as
  `sylvander-ghostty/AGENTS.md`, but Claude Code reads `CLAUDE.md`
  preferentially. Update both if you change one.
- `sylvander-ghostty/macos/AGENTS.md` — macOS `.app` bundle build
  (Xcode, `build.nu`, swiftlint, AppleScript).
- `sylvander-ghostty/example/AGENTS.md` — `example/` sub-projects
  (libghostty-vt examples, Doxygen snippets).

## Project Layout

```
Sylvander/
├── Cargo.toml              # workspace root; pins Rust 1.96
├── .github/                # parent-level CI (Dependabot, fmt, clippy,
│                           # Linux build, clean-artifacts, nix,
│                           # milestone, release-tag)
├── scripts/
│   └── sync-ghostty-subtree.sh  # pulls upstream ghostty, drops files
│                               # that don't apply to this fork (see
│                               # sylvander-ghostty/SYNCUP.md §7.1)
├── docs/                   # architecture notes
│
├── sylvander-protocol/         # cross-language wire types (serde)
├── sylvander-llm-anthropic/    # Anthropic Messages API client
├── sylvander-agent/            # agent loop + tool registry + memory
├── sylvander-runtime/          # boot / engine / session store glue
├── sylvander-server/           # daemon binary
├── sylvander-tui/              # terminal-UI client
├── sylvander-channel/          # `Channel` trait
├── sylvander-channel-dingtalk/ # DingTalk bot
├── sylvander-channel-http/     # HTTP debug / webhook
├── sylvander-channel-unix/     # Unix-domain socket
├── sylvander-channel-ws/       # WebSocket (used by macOS app)
├── sylvander-channel-telegram/ # Telegram bot
└── sylvander-channel-wechat/   # WeChat Work bot
```

`Sylvander/` is **not** a monorepo of independent projects — it is
one product (the Sylvander agent). The Rust crates are layered:

```
                sylvander-server (binary)
                       │
                       ▼
                sylvander-runtime
                       │
                       ▼
                sylvander-agent ◀──── sylvander-llm-anthropic
                       │           ◀──── sylvander-protocol
                       ▼
                sylvander-channel-*
                       │
                       ▼
                  (bus / bus users)
```

`sylvander-ghostty/` is a **git subtree** of
[ghostty-org/ghostty](https://github.com/ghostty-org/ghostty) with
our rebrand patches on top. It hosts the macOS `.app` (Swift /
AppKit / SwiftUI) and the shared Zig terminal core. It is the
**substrate** Sylvander runs on; it does not contain Sylvander's
business logic.

## Commands (top-level)

### Rust workspace

- **Build:** `cargo build --workspace --locked`
- **Test (lib):** `cargo test --workspace --locked --lib`
  - Some integration tests in `sylvander-agent/tests/real_use_case.rs`
    assert on the streaming-event contract and currently fail when
    the mock response shape drifts. CI opts them out with
    `--skip real_use_case --skip …`. Run them locally to validate.
- **Test (all targets, including integration):** `cargo test --workspace`
  - CI for `cargo test` lives at `.github/workflows/ci.yml::rust`.
- **Lint:** `cargo clippy --workspace --all-targets --locked -- -D warnings`
- **Format check:** `cargo fmt --all -- --check`
- **Apply format:** `cargo fmt --all`

### Zig (in the `sylvander-ghostty/` subtree)

See **`sylvander-ghostty/AGENTS.md`** for the full Zig build matrix.
Quickest gates that work without macOS 26 SDK:

- `zig build -Dapp-runtime=none -Demit-xcframework=false -Demit-macos-app=false`
- `zig build test -Dtest-filter=<name>`

### macOS app (also in `sylvander-ghostty/`)

See **`sylvander-ghostty/macos/AGENTS.md`** for the full Xcode /
`build.nu` workflow. Quickest:

- Build: `macos/build.nu --scheme Sylvander --configuration Release`
  → `sylvander-ghostty/macos/build/Release/Sylvander.app`
- Format / lint: `swiftlint lint --strict --fix`

### Sync upstream ghostty

- `scripts/sync-ghostty-subtree.sh` — pulls
  `ghostty-org/ghostty@main` into `sylvander-ghostty/`, then drops
  upstream-only files (`.github/`, `.agents/`, community docs,
  CMake packaging, vouch system). The full drop list is in
  `sylvander-ghostty/SYNCUP.md` §7.1. Use `--dry-run` first.

## Issue and PR Guidelines

- **Never** create an issue, PR, or release tag without explicit
  user confirmation. The auto-driver script will not help here.
- Before opening a PR, run all local gates locally:
  `cargo test --workspace`, `cargo fmt --all -- --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `zig build -Dapp-runtime=none …`, `macos/build.nu`.
- Commit messages follow the `<emoji> <type>: <short title>`
  format. The `mz-commit` skill is the source of truth for the
  schema; consult it on every commit.
- **Never** edit `sylvander-ghostty/.github/`, `.agents/`,
  `sylvander-ghostty/PACKAGING.md`, `HACKING.md`, `CONTRIBUTING.md`,
  `AI_POLICY.md`, or `VOUCHED.td` — these are upstream-only and
  are dropped on every sync. The full list lives in
  `sylvander-ghostty/SYNCUP.md` §7.1.
- **Never** push to `ghostty-org/ghostty` (we don't have access, and
  we don't need to). The only remote is `christmic/Sylvander`.

## What the agent loop looks like (mental model)

For a working understanding of the agent side, see `docs/` and
the `sylvander-agent/` crate. The relevant pieces:

- `AgentLoop` (`sylvander-agent/src/loop_.rs`) is the async driver
  that calls the Anthropic API, executes tools, re-feeds results,
  and emits `AgentEvent`s.
- `ToolContext` (`sylvander-agent/src/tool_context.rs`) is passed
  to every `Tool::execute` and carries a `SessionContext` (identity,
  origin, request metadata, attributes) plus an
  `ExecutionBudget` (timeout, retries) and a `SurfaceView`
  (filesystem root, capability set, network policy). New tools
  should consult `ctx.has_cap(...)` before doing anything.
- `SessionStore` (`sylvander-agent/src/session_store/`) persists
  session metadata + per-message history. The SQLite backend is
  the only one; the in-memory backend was removed. A new
  `SessionContext` is stored on every `append_message` and used to
  scope `read_history` / `list` / `search`.

## What you should NOT do

- Don't `git push --force` to `christmic/Sylvander@master` without
  asking.
- Don't delete files inside `sylvander-ghostty/` without
  understanding whether they are re-introduced by the next
  `scripts/sync-ghostty-subtree.sh` run. Anything in the drop list
  (§7.1) is safe to delete; anything else might be ghostty's own
  code that the fork still depends on.
- Don't run `git subtree pull` by hand — use the script. The script
  does the same thing plus the cleanup, and folds the cleanup
  into the same commit so the PR history is clean.
