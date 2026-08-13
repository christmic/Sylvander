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

It does not cover:

## 2. Repo layout

The master tree is laid out as one product with layered Rust crates and
one Zig subtree. The full tree-with-explanations lives in
[AGENTS.md](../AGENTS.md); the summary is:

## 3. Toolchain

The pinned versions for this repo:

## 4. `rust-toolchain.toml` and toolchain pinning

The master `Cargo.toml` declares `rust-version = "1.96"` under
`[workspace.package]`. Active CI requests Rust `1.96` explicitly. Developers
may use a newer stable compiler for exploratory work, but closure evidence
must include the pinned 1.96 toolchain or a deliberate workspace-wide update.

The recommended local pin is a per-directory `rust-toolchain.toml`
containing `channel = "1.96"` so `rustup` picks the same compiler as CI.
Do not silently replace that pin with `stable`; a toolchain bump must update
the workspace declaration, CI, release workflow, docs, and lockfile together.

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

To produce a daemon binary that matches clean-room verification:

```sh
cargo install --path sylvander-server --locked --offline --force
cargo install --path sylvander-tui    --locked --offline --force
```

(Used by `scripts/clean-room-verify.sh`.)

## 6. Test commands

The Rust CI and release gate run the same complete workspace suite without
name-based exclusions:

```sh
INSTA_UPDATE=no cargo test --workspace --locked -- --test-threads=1
```

**Do not** set `INSTA_UPDATE=anything` — it silently
regenerates TUI visual layout snapshots. `INSTA_UPDATE=no` makes drift
fail instead.

TUI snapshot drift is its own gate:

```sh
INSTA_UPDATE=no cargo test -p sylvander-tui --test snapshots --locked
```

Rust test bodies live under each owning crate's `tests/` tree; see
[`rust-test-layout.md`](rust-test-layout.md). Recovery and release-recovery
tests should be run before each release (see [§21](#21-release-drill)).

## 7. Lint / format

CI enforces both, and both must pass:

```sh
# Format check (CI: rust-fmt job)
cargo fmt --all -- --check

# Apply format locally before committing
cargo fmt --all

# Clippy with -D warnings (CI: rust-clippy job)
cargo clippy --workspace --all-targets --locked -- -D warnings

# First-party crate-boundary index and maintained relative links
./scripts/verify-docs.sh

# Architectural dependency and boundary-source invariants
./scripts/verify-architecture.sh
```

Workspace lints are declared in `[workspace.lints.rust]` and
`[workspace.lints.clippy]` in `Cargo.toml`:
`unsafe_code = "deny"`, `unreachable_pub = "warn"`, and a pedantic clippy
set with module-repetition and over-bool exceptions deliberately
relaxed.

### Rust import placement

All namespace imports belong in the module import section. A `use` declaration
inside a function, method, `impl`, match arm, test body, or other nested scope is
forbidden. Use a fully qualified path if a module-level short name would be
ambiguous. The only exception is a compiler, macro-expansion, or
conditional-compilation constraint that makes module-level import impossible;
such an exception must include an English `// Local import required: ...`
comment. See [`AGENTS.md`](../AGENTS.md) for the normative rule and verification
requirements.

## 8. Automation boundary

The slim repository currently contains no tracked `.github/workflows/`
directory. Do not cite historical desktop, Zig, Nix, notarization, or artifact
jobs as current verification. The authoritative local gates are the commands
in `docs/release-closure.md` and the maintained scripts under `scripts/`.

If CI is introduced later, it must execute those same gates against one exact
commit. A disabled or advisory job is never release evidence.

## 9. Local verification scripts

The maintained verification scripts under `scripts/` each gate one bounded
part of the repository contract. The release closure document defines which
ones are required together for a release claim.

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

1. Runs `verify-architecture.sh` before security checks so a release cannot
   join Agent/API outside Runtime or reintroduce runtime dependencies in the
   public protocol.
2. `git grep` for high-confidence secret patterns (`sk-...`, AWS keys,
   `BEGIN ... PRIVATE KEY`, `gh[pousr]_...`). One known false-positive
   in `sylvander-tui/src/tool_presenter.rs:1151` is whitelisted via
   `grep -v`.
3. `cargo metadata --locked --no-deps` to confirm the lockfile parses
   without network.
4. Resolves `cargo-audit` (system or `~/.cargo/bin`) and runs
   `cargo audit --no-yanked` with the cargo proxy cleared.
5. Runs ten cross-cutting security tests covering malformed protocol
   input (`sylvander-api`), path/command-argument injection and
   cross-owner isolation (`sylvander-agent`), profile and restart
   isolation (`sylvander-runtime`), socket credentials and live-event
   isolation (`sylvander-channel-unix`), and secret redaction
   (`sylvander-tui`).

Pass = "security verification passed".

### verify-architecture.sh

Reads locked Cargo metadata and fails when Agent has a first-party dependency
other than `sylvander-llm-core`, Protocol gains runtime/infrastructure
dependencies, a Channel depends on Agent, a provider adapter depends on another
first-party layer, or a crate other than Runtime joins Agent and Protocol. It
also rejects the removed Protocol `types` path and nested Rust `use`
declarations in Agent, Channel, and Protocol sources.

Pass = both dependency-graph and boundary-source verification messages.

### verify-docs.sh

Pass = a count of verified crate boundaries and maintained Markdown files.

### verify-rust-test-layout.sh

Rejects Rust test bodies, inline `mod tests { ... }` blocks, and test files
under any crate's `src/` tree. `scripts/tests/verify-rust-test-layout.sh`
exercises the guard itself against a nested-crate fixture so a traversal
regression cannot make the policy silently pass.

Pass = both the repository layout guard and its regression fixture complete.

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

Tools implement `ToolDefinition` and `ToolExecutor` in
`sylvander-agent/src/tool/contract.rs`. Definition and preparation are synchronous;
execution is `async_trait`-bound for dyn-compatibility and Send safety. The
per-call context is `ToolContext`.

### Skeleton

```rust
use async_trait::async_trait;
use serde_json::json;
use sylvander_agent::tool::{
    PreparedToolCall, ToolDefinition, ToolError, ToolExecutor, ToolOutput,
    ToolSpec,
};
use sylvander_agent::tool_context::ToolContext;
use sylvander_agent::tool_invocation::ToolInvocationClass;

pub struct MyTool;

impl ToolDefinition for MyTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::immediate(
            "my_tool",
            "Return one bounded project summary",
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
            ToolInvocationClass::Read,
        )
    }
}

#[async_trait]
impl ToolExecutor for MyTool {
    async fn handle(
        &self,
        ctx: &ToolContext,
        _call: &PreparedToolCall,
    ) -> Result<ToolOutput, ToolError> {
        if !ctx.has_cap(sylvander_agent::tool_context::Cap::Read) {
            return Ok(ToolOutput::err("read capability not granted"));
        }
        Ok(ToolOutput::ok("bounded summary"))
    }
}
```

Tools that produce incremental output override `handle_streaming` and emit
through its `ToolProgressSink`. Runtime never calls an executor with raw model
input.

Preparation validates the declared schema shape, freezes normalized input,
coordination mode, and `ToolExecutionPolicy`, and only then enters the
authorization gateway. A tool that launches a process must prepare either
`ToolExecutionPolicy::process()` or
`ToolExecutionPolicy::read_only_process()`. Such a call fails closed unless
the selected executor reports OS-enforced filesystem isolation, denied
network, and resource limits. The current enforcing backend is the explicit
OCI `container` transport; `local` and `ssh` do not claim sandboxing.

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

Executor contracts live in `sylvander-agent/src/execution/workspace.rs`;
concrete local, SSH, and OCI adapters live under `sylvander-runtime/src/execution/`.
Runtime selects an adapter and dispatches workspace operations to the bound
target. The full contract lives in
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

The wiring sits next to the `local`, `ssh`, and `container` arms in the
executor factory. The server configuration adds a new variant if a new
transport requires a new TOML shape. A new executor must leave
`process_isolation()` at its unconfined default until every reported property
is enforced by its concrete operating-system or container boundary.

## 13. Adding MCP / Skill

Sylvander treats MCP servers as supervised external tool sources and
Skill packages as workspace-scoped instruction bundles. Both have
dedicated docs that are authoritative:

- MCP Session ownership, persistent sandbox, frames, health, and reconnection:
  [`sylvander-runtime/docs/mcp.md`](../sylvander-runtime/docs/mcp.md).
- Skill packages, manifest schema, activation, and the
  per-turn budget:
  [`sylvander-agent/docs/skills.md`](../sylvander-agent/docs/skills.md).

For a local stdio MCP server, declare its required `execution_environment`,
explicit `workspace_access`, `command`, and secret references in the Agent
definition. The target must resolve to a Runtime-owned persistent environment
that proves filesystem, network, resource, and process-tree isolation. An
unknown, local-unconfined, or unavailable target fails before process creation;
there is no host fallback. For a remote server, declare
`type = "mcp_streamable_http"`, one HTTPS `url`, and an optional Runtime secret
reference in `bearer_token`; the remote endpoint receives no local workspace
or process authority. When you ship a new Skill directive, place it under
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
- Current-schema rollout and rollback guidance belongs at the bottom of
  [`server-configuration.md §Stable user identity binding`](server-configuration.md)
  so operators see it during a deploy. Add compatibility/migration guidance
  only when the task explicitly approves the source version and transition.

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

## 16. Pre-release version policy

Sylvander has not shipped a compatibility promise. Unless a task explicitly
names an old version that must remain supported, change the interface,
callers, fixtures, generated schemas, tests, examples, and documentation to
the latest contract in the same bounded change.

- Do not add fallback decoders, dual read/write paths, silent repair,
  downgrade behavior, or migration adapters “just in case”.
- Old, unknown, or damaged schemas fail closed with a stable content-safe
  error. Never guess which current representation an old payload intended.
- Production state uses the durable Runtime-selected backend. In-memory
  implementations are test fixtures only; they are not a server mode, a
  production configuration value, or a fallback.
- A compatibility exception must state the exact source version, supported
  transition, removal gate, and acceptance tests before implementation.
- Git history and small reversible commits are the rollback path before the
  first release.

## 17. Schema evolution (`sylvander-api` codegen)

The protocol crate is the cross-language wire-type root. It is hand
maintained, not `protoc`-generated. Under the latest-only policy:

## 18. Configuration schema

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

## 19. Logging & tracing conventions

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

## 20. Common pitfalls

The project's authoritative list lives in
[AGENTS.md §"What you should NOT do"](../AGENTS.md). Reproduced in
summary:

Verification gotchas worth restating:

- `INSTA_UPDATE=no` is required for the snapshot job — setting
  `INSTA_UPDATE=anything` silently regenerates visual layout, and
  next month's PR will get random layout shifts.
- The workspace test gate has no name-based skip list. If a provider fixture
  drifts, update the fixture or the current provider contract in the same
  bounded change; do not hide the failure behind `--skip`.
- Protocol contract tests use local mock HTTP/SSE servers and require no live
  provider credential. Credential-gated tests remain supplemental evidence.
- Rust test bodies belong under each crate's `tests/` tree. Production modules
  may expose a test-only `#[path = "../tests/unit/…"]` bridge for white-box
  access; never put test bodies back under `src/`.
- No tracked CI workflow currently substitutes for running the documented
  local release gates.

## 21. Release drill

A release drill walks the recovery and security gate end-to-end on a
clean checkout. Source of truth:

- [`release-closure.md`](release-closure.md) — the supported release
  scope, the reproducible gate commands, and the residual-risk
  non-claims.
- [`recovery-drills.md`](recovery-drills.md) — the registry, session,
  channel, worktree, memory, and release-recovery drill commands.

Use them as written. Do not invent a "shorter" gate for convenience —
the closure record is the legal claim of what the release can do.
