# Developer manual

This manual is for engineers extending Sylvander or auditing its
implementation. It complements, never replaces, the per-crate
documentation under `sylvander-runtime/docs/` and
`sylvander-agent/docs/`. When this manual and a crate's own docs
disagree, **the crate's own docs win** for that crate.

---

## 1. Audience & scope

This manual assumes:

- Comfortable with the Rust 2024 edition and async `tokio` idioms.
- Familiarity with TOML, SQLite, and Unix process conventions.
- Production-grade operator hygiene (no committed secrets, no
  speculative APIs, fail-closed when the contract is unclear).

It covers:

- The Sylvander Rust workspace (server, agent, runtime, channels, TUI).
- The ghostty `sylvander-ghostty/` subtree and its role as substrate.
- The CI workflows that gate every PR.

It does not cover:

- The Anthropic Messages API contract — see
  `sylvander-llm-anthropic/docs/`.
- macOS `.app` packaging internals — see
  `sylvander-ghostty/macos/AGENTS.md`.
- Production data-backup strategy beyond what is enforced by the
  integrity anchor (see [§20](#20-release-drill)).

## 2. Repo layout

The master tree is laid out as one product with layered Rust crates and
one Zig subtree. The full tree-with-explanations lives in
[AGENTS.md](../AGENTS.md); the summary is:

```
Sylvander/
├── Cargo.toml              # workspace root; pins Rust 1.96
├── AGENTS.md               # top-level agent guide (read first)
├── .github/                # CI workflows (CI, clean-artifacts,
│                           # milestone, nix, release-tag)
├── scripts/                # clean-room / performance / security /
│                           # ghostty-subtree shell scripts
├── docs/                   # architecture & manual docs (this file)
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

The Rust crates form a strict, downward dependency graph (see
[AGENTS.md §"Project Layout"](../AGENTS.md) for the ASCII diagram). The
ghostty subtree is a fork with rebrand patches; do not edit files listed
in `sylvander-ghostty/SYNCUP.md §7.1`.

## 3. Toolchain

The pinned versions for this repo:

| Tool       | Version              | Source                                   |
| ---------- | -------------------- | ---------------------------------------- |
| Rust       | 1.96 (MSRV)          | `[workspace.package].rust-version`       |
| Zig        | 0.15.2               | `ci.yml` env, `nix.yml`, `release-tag.yml`|
| Xcode      | 26 (beta runner)     | `zig-full` job, `release-tag` job        |
| macOS SDK  | 26                   | `macos-26-xlarge` runner                 |
| OpenSSL    | libssl-dev (Linux)   | `rust-linux` job apt-get line            |
| SQLite     | libsqlite3-dev       | `rust-linux`, `tui-snapshots` jobs       |
| protoc     | protobuf-compiler    | same jobs as above                       |

CI installs Zig with `mlugg/setup-zig@v2` and pins Xcode with
`ls -d /Applications/Xcode_26*.app | head -n 1` plus
`sudo xcode-select -s`. The Rust toolchain uses
`dtolnay/rust-toolchain@stable` plus rustfmt/clippy components; the
MSRV is enforced through `rust-version` in `[workspace.package]`.

## 4. `rust-toolchain.toml` and toolchain pinning

The master `Cargo.toml` declares `rust-version = "1.96"` under
`[workspace.package]`. The actual toolchain used by every developer is
`stable` (matching the CI's `dtolnay/rust-toolchain@stable`).

The recommended local pin is a per-directory `rust-toolchain.toml`
containing `channel = "stable"` so `rustup` always picks the same
compiler CI uses. **Do not** write `channel = "1.96"` — CI never
requests that channel and the stable channel is what catches drift.

If you need to temporarily try a different toolchain, use
`rustup override set <toolchain>` in your shell, never in committed
files.

## 5. Build commands

From the repository root:

```sh
# Workspace build (locked, mirrors CI 'rust' job)
cargo build --workspace --locked

# Same, release profile (used by performance-verify.sh)
cargo build --workspace --release --locked
```

`--locked` is mandatory in CI and recommended locally; it stops the
build if `Cargo.lock` would otherwise change. The Zig
`sylvander-ghostty/` subtree is built and tested separately through CI;
see `sylvander-ghostty/AGENTS.md` for its matrix.

To produce a daemon binary that matches clean-room verification:

```sh
cargo install --path sylvander-server --locked --offline --force
cargo install --path sylvander-tui    --locked --offline --force
```

(Used by `scripts/clean-room-verify.sh`.)

## 6. Test commands

CI runs tests with most streaming-event contract tests opted out
because their mock-response shape drifts. The same `cargo test` flags
reproduce CI exactly:

```sh
INSTA_UPDATE=no cargo test --workspace --locked -- \
  --skip real_use_case \
  --skip single_iteration_end_turn_returns_final_message \
  --skip event_order_iteration_start_chunks_end \
  --skip max_iterations_limit_enforced \
  --skip stream_wrong_content_type_errors
```

To run the full suite including the skipped contract tests, drop the
`--skip` filters. **Do not** set `INSTA_UPDATE=anything` — it silently
regenerates TUI visual layout snapshots. `INSTA_UPDATE=no` makes drift
fail instead.

TUI snapshot drift is its own gate:

```sh
INSTA_UPDATE=no cargo test -p sylvander-tui --test snapshots --locked
```

See [§19](#19-common-pitfalls) for the deferred test-relocation note;
the recovery and release-recovery tests live under
`sylvander-runtime --lib` and should be run before each release (see
[§20](#20-release-drill)).

## 7. Lint / format

CI enforces both, and both must pass:

```sh
# Format check (CI: rust-fmt job)
cargo fmt --all -- --check

# Apply format locally before committing
cargo fmt --all

# Clippy with -D warnings (CI: rust-clippy job)
cargo clippy --workspace --all-targets --locked -- -D warnings
```

Workspace lints are declared in `[workspace.lints.rust]` and
`[workspace.lints.clippy]` in `Cargo.toml`:
`unsafe_code = "deny"`, `unreachable_pub = "warn"`, and a pedantic clippy
set with module-repetition and over-bool exceptions deliberately
relaxed.

## 8. CI workflow tour

The repo uses five workflows under `.github/workflows/`.

### ci.yml (`CI`, multi-job)

Triggers on push to master, pull_request, and `workflow_dispatch`. Jobs:

- **zig-module** — `macos-latest`. Runs `zig build test --summary all`
  inside `sylvander-ghostty/src/sylvander/`; smoke test of the Sylvander
  Zig module.
- **zig-checked** — `macos-latest`. `zig build -Dapp-runtime=none
  -Demit-xcframework=false -Demit-macos-app=false`. Catches rebrand
  breakage, syntax errors, and API drift without requiring the macOS 26
  SDK.
- **zig-full** — `macos-26-xlarge`. `zig build -Dapp-runtime=none test`.
  Requires Xcode 26. Pin selected via `xcode-select -s` after locating
  `/Applications/Xcode_26*.app`.
- **rust** — `macos-latest`. Runs `cargo build --workspace --locked`,
  the macOS app helper contract via `build-sylvander-tui-universal.sh`
  and `embed-sylvander-tui.sh`, plus `cargo test --workspace --locked`
  with the skip list above. Uploads universal-helper lipo / codesign
  checks; secrets: none directly.
- **macos-swift** — `macos-latest`. Verifies `Sylvander-Info.plist`,
  `Sylvander.sdef`, and `Sylvander.xcodeproj/project.pbxproj` exist and
  have no `Ghostty-Info.plist` / `Ghostty.sdef` references.
- **rust-fmt** — `macos-latest`. `cargo fmt --all -- --check`.
- **rust-clippy** — `macos-latest`. `cargo clippy --workspace
  --all-targets --locked -- -D warnings`. Catches dead code and unused
  imports before they leak to a release tag.
- **rust-linux** — `ubuntu-latest`. `cargo build --workspace --locked`
  after `apt-get install libssl-dev libsqlite3-dev
  protobuf-compiler`. Catches macOS-only assumptions (Hardcoded
  `/Users/foo`, Apple-only crates).
- **tui-snapshots** — `ubuntu-latest`. `INSTA_UPDATE=no cargo test
  -p sylvander-tui --test snapshots --locked`; uploads any
  `.snap.new` artifacts on failure.

Zig version is controlled by an `env: ZIG_VERSION: "0.15.2"` block at
the workflow top; cache is `cache: false` so network drift can't be
masked.

### clean-artifacts.yml (`Clean old artifacts`)

Triggers weekly (`cron: "0 3 * * 0"`) and manually. Required secret:
`GITHUB_TOKEN` (with `actions: write`). Walks the artifact list paginated,
filters by `created_at`, `DELETE`s anything older than 14 days. Intended
to offset the per-minute cost of `macos-26-xlarge` jobs.

### milestone.yml (`Milestone sync`)

Triggers when a PR is `closed` (and merges). Required secret:
`GITHUB_TOKEN` (`issues: write`). Parses `Closes #N`, `Fixes #N`,
`Resolves #N` from the PR body; picks the open milestone with the
lowest number; assigns each linked issue to that milestone.

### nix.yml (`Nix shell build`)

Triggers on push to master and PR. Runs inside `nix develop` on
`macos-26-xlarge`. Installs `nixpkgs#zig_0_15` and runs the minimal zig
build. Catches the case where a contributor's local Zig (brew zig
0.15.2, system sqlite) silently diverges from the project deps. We do
**not** use a CACHIX account; the Cachix step is intentionally empty.

### release-tag.yml (`Release tag`)

Triggers on `v*.*.*` tag pushes and `workflow_dispatch`. Required
secrets:

- `MACOS_CERTIFICATE_P12`, `MACOS_CERTIFICATE_PASSWORD` — Developer ID
  certificate.
- `MACOS_SIGNING_IDENTITY` — `codesign -s` identity.
- `APPLE_ID`, `APPLE_TEAM_ID`, `APPLE_APP_PASSWORD` — notarytool
  credentials.

Builds the universal `.app` via `zig build` + `xcodebuild archive`,
notarizes with `xcrun notarytool submit --wait`, staples with `xcrun
stapler`, validates with `xcrun stapler validate` and `spctl
--assess`. Uploads `Sylvander.app.zip` plus sha256 (30-day retention)
and creates a **draft** GitHub Release.

`concurrency.cancel-in-progress` is `false` here — never cancel a
release in progress.

## 9. Local verification scripts

Three scripts live in `scripts/`. Each is a shell entry point that
gates one aspect of the release claim.

### clean-room-verify.sh

End-to-end check that the released binary boots, serves traffic, and
shuts down cleanly from a fresh config:

1. Archives `HEAD` into a `mktemp -d` working directory.
2. Sets `CARGO_TARGET_DIR` to a dedicated clean-room target dir and
   unsets the cargo proxies.
3. Runs `cargo install --path sylvander-server --root <room> --locked
   --offline --force` and the same for `sylvander-tui`.
4. Writes a fresh `server.toml` (terminal-channel, fixture-model
   provider, local execution target) into the room.
5. Starts the installed `sylvander` binary, polls for the Unix socket
   (up to 100 × 50 ms), checks `kill -0` on the PID, asserts
   `sessions.db` and `memory.db` exist.
6. Sends `SIGINT` and requires a clean `wait`.

Pass = "clean-room install, startup, readiness, and shutdown passed".

### performance-verify.sh

Time-bound sanity check over the locked release build:

1. `cargo build --workspace --release --locked`.
2. Prewarms specific test binaries (compilation time is not budgeted,
   only runtime is).
3. Runs eight test invocations, each with a 10-second budget:
   message-bus burst, large-workspace bounds, concurrent tool
   scheduling, tool-progress burst, long TUI transcript retention, TUI
   input flood, TUI service backpressure, container resource ceilings.
4. Exits non-zero on any budget overrun.

Pass = "local performance verification passed".

### security-verify.sh

Security claim coverage:

1. `git grep` for high-confidence secret patterns (`sk-...`, AWS keys,
   `BEGIN ... PRIVATE KEY`, `gh[pousr]_...`). One known false-positive
   in `sylvander-tui/src/tool_presenter.rs:1151` is whitelisted via
   `grep -v`.
2. `cargo metadata --locked --no-deps` to confirm the lockfile parses
   without network.
3. Resolves `cargo-audit` (system or `~/.cargo/bin`) and runs
   `cargo audit --no-yanked` with the cargo proxy cleared.
4. Runs ten cross-cutting security tests covering malformed protocol
   input (`sylvander-protocol`), path/command-argument injection and
   cross-owner isolation (`sylvander-agent`), profile and restart
   isolation (`sylvander-runtime`), socket credentials and live-event
   isolation (`sylvander-channel-unix`), and secret redaction
   (`sylvander-tui`).

Pass = "security verification passed".

## 10. Adding a new channel crate

Channel adapters implement the `Channel` trait from `sylvander-channel`
and connect the server to a transport. Concrete crates
(`sylvander-channel-http`, `-unix`, `-ws`, `-dingtalk`, `-telegram`,
`-wechat`) live as siblings under the workspace root and are declared
in `[workspace] members` of the root `Cargo.toml`.

### Skeleton

Create a new crate at `sylvander-channel-<kind>/`:

```toml
# sylvander-channel-<kind>/Cargo.toml
[package]
name = "sylvander-channel-<kind>"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
sylvander-channel.workspace = true
sylvander-agent.workspace = true
# transport-specific deps (reqwest, tokio-tungstenite, axum, …)
```

The crate exports one public constructor returning
`Arc<dyn Channel>` and one or more config knobs that map to a
`ChannelTransportConfig::Variant { … }` arm in
`sylvander-runtime::config`.

### Where to register

`sylvander-server/src/main.rs::build_channels` is the single map from
`ChannelTransportConfig` variants to concrete channel constructors.
Add a new arm that:

1. Resolves any `SecretRef`s (`app_key`, `app_secret`, `bearer_token`,
   etc.) via `resolve_text(&secrets, …)`.
2. Builds an `Arc<dyn Channel>` with the configured `.id` /
   `default_agent` and any request limits.
3. Returns the registration; `Runtime::start_channels` takes care of
   the lifecycle.

The runtime needs no further wiring — it consumes the `Vec<ChannelRegistration>`
that `build_channels` returns.

### Conformance checklist

- Implements `Channel::serve` with bounded read/write budgets.
- Surfaces `OperationalHealth` (if the channel has external lifecycle
  state) via the same `OperationalHealth`-providing pattern as
  `sylvander-channel-http`.
- Honors `channels.supervision` (`max_restart_attempts`,
  `initial_backoff_ms`, `max_backoff_ms`) declared per instance in
  the TOML config.
- Maps the authenticated inbound identity to a principal per
  [`boundary-authorization.md`](boundary-authorization.md) before
  issuing an Agent request.

## 11. Adding a new tool

Tools implement `Tool` in `sylvander-agent/src/tool.rs`. The trait is
`async_trait`-bound for dyn-compatibility + Send safety. The per-call
context is `ToolContext` (see
`sylvander-agent/src/tool_context.rs`).

### Skeleton

```rust
use std::sync::Arc;
use async_trait::async_trait;
use serde_json::Value as JsonValue;
use sylvander_llm_anthropic::api::types::InputSchema;
use sylvander_agent::tool::{Tool, ToolOutput};
use sylvander_agent::tool_context::ToolContext;

pub struct MyTool;

#[async_trait]
impl Tool for MyTool {
    fn name(&self) -> &'static str { "my_tool" }

    fn schema(&self) -> InputSchema {
        // JSON Schema describing call input.
        todo!()
    }

    async fn execute(
        &self,
        ctx: Arc<ToolContext>,
        input: JsonValue,
        sink: ToolProgressSink,
    ) -> Result<ToolOutput, ToolError> {
        // Consult ctx.session.identity, ctx.surface, ctx.budget.
        // Use ctx.executor for any workspace operation.
        todo!()
    }
}
```

### Context hygiene

Every tool must:

- Read `ctx.session.identity.{user_id, agent_id, session_id}` for
  namespacing and access control.
- Use `ctx.executor` (a `WorkspaceExecutor`) for any filesystem or
  command operation; never call std fs / command APIs directly.
- Check `ctx.surface.capabilities` for the operations it needs and
  refuse rather than escalate.
- Honor `ctx.budget.timeout`; cancel any spawned process on drop.

### Registration

Register the tool in `sylvander-agent`'s `ToolRegistry` so the agent loop
picks it up. Keep tool-specific config in the Agent definition; do not
statically couple a tool to a hard-coded model or provider.

## 12. Adding a new executor

Executors implement `WorkspaceExecutor` in
`sylvander-agent/src/workspace_executor.rs`. They dispatch workspace
operations to local, SSH, container, or sandbox targets. The full
contract lives in
[`workspace-execution.md`](../sylvander-agent/docs/workspace-execution.md).

A new executor must:

- Resolve to a typed `WorkspaceTarget` carrying the execution target
  ID and binding (path or remote URI).
- Bound every operation by `ExecutionBudget` timeout and any
  per-target resource ceiling.
- Stream stdout/stderr via `WorkspaceCommandStream` so the agent loop
  can apply its head/tail capture and live-progress policy.
- Honor cancellation by killing the owned child process on drop.
- Reject any operation whose capability isn't granted by
  `ctx.surface.capabilities` (file_access, network_access, command).

The wiring sits next to `local`, `ssh`, `container`, and `sandbox`
arms in the executor factory. The server configuration adds a new
variant if a new transport requires a new TOML shape.

## 13. Adding MCP / Skill

Sylvander treats MCP servers as supervised external tool sources and
Skill packages as workspace-scoped instruction bundles. Both have
dedicated docs that are authoritative:

- MCP runtime lifecycle, frames, health, reconnection:
  [`sylvander-agent/docs/mcp.md`](../sylvander-agent/docs/mcp.md).
- Skill packages, manifest schema, activation, and the
  per-turn budget:
  [`sylvander-agent/docs/skills.md`](../sylvander-agent/docs/skills.md).

When you wire a new MCP server, match the existing pattern: declare
the entry in the Agent TOML, resolve its `command` and any secrets
through `SecretRef`, and let the runtime own the child process via
kill-on-drop. When you ship a new Skill directive, place it under
`.agents/skills/` (Agent home trust) or `.sylvander/skills/` /
`skills/` (task workspace trust), and keep the SKILL.md under 16 KiB
to fit the shared `48 KiB / 24-document` budget.

## 14. Boundary authorization changes

Authorization boundary changes are wire-contract changes. Treat them
like schema evolution:

- New admission rules land in `sylvander-channel-*` and are tested
  against the bearer/principal/`X-Telegram-Bot-Api-Secret-Token`
  contracts documented in [`boundary-authorization.md`](boundary-authorization.md).
- Authorization audit entries must include `redacted` rationale
  (never the offending payload) and a typed outcome
  (Allow / Deny / ApproveRequired).
- Update the Agent access policy tests under
  `sylvander-agent --lib boundary` if the cross-owner isolation rules
  change.
- Migration guidance belongs at the bottom of
  [`server-configuration.md §Stable user identity binding`](server-configuration.md)
  so operators see it during a deploy.

## 15. Identity binding changes

[`identity-binding-protocol.md`](identity-binding-protocol.md) is the
source of truth. When extending it:

- The digest key length, TTL bounds (30–900 s), and "trusted issuer
  triple" uniqueness rule are load-bearing; changing them is a
  breaking change for every existing issuer.
- The runtime owns a latest-schema SQLite store at
  `server.identity.database` (default `<data_dir>/identity.db`).
  Adding or removing a column requires version-the-schema
  documentation in the protocol doc.
- Resolve and CAS unlink must always operate on the
  **authenticated ingress-derived external identity**, never a
  client-supplied string. New entry points should reject any input
  that carries a `user`, `transport`, or `external_principal_id`
  field up front.
- Add a recovery test under `sylvander-runtime --lib
  identity_binding` that confirms a restart restores the exact owner
  profile and isolates other users.

## 16. Schema evolution (`sylvander-protocol` codegen)

The protocol crate is the cross-language wire-type root. It is hand
maintained, not `protoc`-generated, but the version flow is identical:

- Bump the package `version` in `sylvander-protocol/Cargo.toml` when
  adding fields.
- Mark newly added fields `#[serde(default)]` so existing clients
  remain decoded.
- Never remove a field without a deprecation cycle recorded in
  `sylvander-protocol/CHANGELOG.md`.
- The CLI / TUI / channels must all be updated together; CI's
  `cargo build --workspace --locked` catches drift but **not**
  semantic drift — write a contract test under
  `sylvander-protocol --lib`.

## 17. Configuration schema

The authoritative reference is
[`server-configuration.md`](server-configuration.md). The maintained
example at [`config/sylvander.example.toml`](../config/sylvander.example.toml)
mirrors the v1 schema. When extending it:

- Unknown fields fail startup — be deliberate about every field name.
- Resolved secrets flow through `SecretRef` (`source = "env" | "file"`
  only). Secret **values** must never appear in Debug, errors, or
  command lines.
- Bound every numeric field (timeouts, retries, batch sizes, TTL
  windows, etc.) at startup validation.
- Pair any new optional section with an explicit default the server
  applies when the field is absent — leave no field "implicitly
  pulled from somewhere".
- Test by feeding the example config through `clean-room-verify.sh`.

## 18. Logging & tracing conventions

Sylvander uses `tracing` everywhere. The server initializes the
subscriber in `sylvander-server/src/main.rs::init_tracing`:

- Default level is `info` unless `RUST_LOG` overrides via
  `EnvFilter::try_from_default_env`.
- JSON output is opt-in via `SYLVANDER_LOG_FORMAT=json` (uses
  `.json().flatten_event(true)`).
- Use structured fields, not string interpolation, for searchable
  values: `tracing::info!(server = %name, "boot completed")`.
- Never log secret values, raw tool I/O, or unredacted prompts.
  Secret resolvers in `sylvander-runtime::config` already redact via
  `Debug`; mirror that pattern when adding new types.
- Channel hot paths should emit only on state transitions, not per
  message — see `sylvander-channel-unix` for the bounded pattern.

## 19. Common pitfalls

The project's authoritative list lives in
[AGENTS.md §"What you should NOT do"](../AGENTS.md). Reproduced in
summary:

- Do not `git push --force` to `christmic/Sylvander@master` without
  asking.
- Do not delete files inside `sylvander-ghostty/` without checking
  the drop list (`sylvander-ghostty/SYNCUP.md §7.1`).
- Do not run `git subtree pull` by hand; use
  `scripts/sync-ghostty-subtree.sh`.

CI gotchas worth restating:

- `INSTA_UPDATE=no` is required for the snapshot job — setting
  `INSTA_UPDATE=anything` silently regenerates visual layout, and
  next month's PR will get random layout shifts.
- The streaming-event skip filters in `cargo test` (see [§6](#6-test-commands))
  exist because mock response shapes drift; do not delete them when
  the failure feels stale. Fix the mock or the contract.
- The CI `rust-linux` job is intentionally build-only — the wiremock
  tests need a running server. Do not add Linux-only test runs in PR
  without confirming the server is reachable from the job.
- Worktree relocation of the doctest suite into `sylvander-runtime --lib`
  is a tracked follow-up; until that lands, keep recovery tests
  physically co-located with the runtime crate.
- Don't try to cache Nix in `nix.yml` — we deliberately do not have a
  CACHIX account.

## 20. Release drill

A release drill walks the recovery and security gate end-to-end on a
clean checkout. Source of truth:

- [`release-closure.md`](release-closure.md) — the supported release
  scope, the reproducible gate commands, and the residual-risk
  non-claims.
- [`recovery-drills.md`](recovery-drills.md) — the registry, session,
  channel, worktree, memory, and release-recovery drill commands.

Use them as written. Do not invent a "shorter" gate for convenience —
the closure record is the legal claim of what the release can do.
