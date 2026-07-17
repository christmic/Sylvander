# Sylvander user manual

Status: active user documentation

Last updated: 2026-07-17

Scope: how to install, configure, run, and use a Sylvander server from an
operator or end-user perspective.

This manual is the operator-facing counterpart to
[`sylvander-agent-platform.md`](sylvander-agent-platform.md) (normative
architecture), [`server-configuration.md`](server-configuration.md)
(configuration schema reference), and
[`operations-runbook.md`](operations-runbook.md) (on-call and triage).

## 1. What Sylvander is

Sylvander is a server-owned Agent platform. One `sylvander` process owns
durable Agent definitions, conversation sessions, memory, run evidence, and
configuration. Multiple clients connect through independent, instance-scoped
channels (TUI over a Unix socket, an HTTP debug API, a WebSocket, or external
chat platforms such as DingTalk, Telegram, and WeChat).

The product contract is:

- one binary (`sylvander`) that is the composition root;
- one authoritative server per environment — clients never embed Agent
  logic;
- one versioned, public service protocol shared by every transport;
- durable Agent identity, prompts, memory, workspaces, and capabilities;
- channels are *instances* (multiple bots of the same platform are separate
  channel entries with their own credentials, sessions, health, and restart
  policy).

For the formal non-negotiable invariants, see `sylvander-agent-platform.md`
§2. For audit status of each subsystem, see the same document §4 and §5.

## 2. Concepts

The vocabulary below is used throughout the configuration, protocol, and
operations documentation.

- **Agent** — a persistent, versioned identity with a persona, default model,
  memory profile, home workspace, capability policy, and tool/Skill/MCP
  extensions. Agents are durable: identity, instructions, memory, and
  workspaces survive server restarts.
- **Session** — one interaction with an Agent. A session overlays model,
  reasoning, task workspace, and execution target on top of the Agent
  defaults. Two clients with two sessions against the same Agent do not
  interfere with each other.
- **Channel** — one ingress adapter instance. Each entry in `[[channels]]`
  binds a transport (Unix socket, HTTP, WebSocket, DingTalk, Telegram,
  WeChat) to a default Agent, a credential reference, and a supervision
  policy.
- **Tool** — a built-in or registered action the Agent can call (Read,
  Write, Edit, List, Search, Command, Git, Skill, MCP). Tools are executed
  by a location-neutral executor against the session's workspace.
- **Skill** — a discoverable package with `SKILL.md` and optional
  `SKILL.toml`, located in an Agent home or task workspace. Skills are
  activated and deactivated explicitly; their instructions are bounded and
  validated.
- **MCP** — Model Context Protocol server, supervised by the Runtime over
  stdio. Each server contributes a namespaced tool and resource catalog.
- **Workspace** — a logical, role-bearing mount (Agent home, task,
  dependency, artifact, scratch). Filesystem paths resolve through the
  configured execution target (`local`, `ssh`, `container`, `sandbox`).
- **Run** — one server-process lifetime, including every turn, step,
  decision, and feedback recorded while it lived. Runs persist in the
  evidence ledger.
- **Evidence** — structured, durable facts about a run: tool calls,
  decisions, outcomes, feedback, and redacted metadata. Evidence is the
  authoritative source for triage, evaluation, and the gated
  self-improvement loop; it is not a transcript dump.

Two adjacent concepts also appear in this manual:

- **Worktree lease** — when a writable Git task workspace is in use,
  Sylvander creates a collision-free branch and worktree directory the
  Agent operates inside. Merge is a separate, reviewable operation.
- **Approval gate** — an Agent may be configured so that sensitive tool
  calls require an explicit, per-session approval from the connected
  client before they execute.

## 3. System requirements

A production Sylvander server needs:

- a Linux or macOS host with a recent stable kernel (Linux 5.x+, macOS 13+);
- a Rust 1.74+ toolchain if you are building from source; prebuilt binaries
  are unsigned and intended for evaluation;
- 2 GiB of RAM for typical single-Agent workloads; raise the ceiling for
  container or sandbox execution targets because each operation allocates
  up to the configured memory limit inside the OCI runtime;
- one writable data directory for the session, evidence, memory, and
  profile databases (defaults under `$XDG_DATA_HOME/sylvander` or
  `~/.local/share/sylvander`);
- one writable directory for the integrity anchor file, outside the data
  directory, on storage the database writer cannot modify (the file
  backend) or an HTTPS CAS endpoint reachable from the host (the HTTP
  backend);
- network access to the Anthropic-compatible upstream
  (`ANTHROPIC_BASE_URL`), plus optional access to MCP servers, the
  external chat platform webhook URLs, and any non-`local` execution
  target;
- OCI runtime (`docker`, `podman`, or compatible) only if you intend to
  use the `container` or `sandbox` execution targets; otherwise it is
  unused.

For evaluation and tutorial use, a single macOS or Linux workstation with
no OCI runtime and no integrity anchor is sufficient — the agent-only
self-use mode skips the integrity anchor.

## 4. Installation

### 4.1 macOS application bundle

A signed `.app` bundle is the recommended distribution for end users on
macOS. Drop it into `/Applications` and launch `Sylvander.app`. The first
launch opens a setup wizard that writes a default `server.toml` into
`~/Library/Application Support/Sylvander` and offers to import an existing
Anthropic API key from the system keychain.

Updates are delivered through the standard macOS update channel once a
stable release is announced.

### 4.2 Standalone daemon from source

Build the server with the workspace's pinned toolchain:

```sh
git clone https://github.com/example/sylvander.git
cd sylvander
cargo install --path sylvander-server --locked
```

`cargo install` places the `sylvander` binary on your `PATH`
(`~/.cargo/bin/sylvander` by default). The `--locked` flag pins
`Cargo.lock`; remove it only when you intentionally want to update
dependencies.

A development build (faster compile, slower runtime) is available with
`cargo build --release -p sylvander-server` from the workspace root; the
artifact lives at `target/release/sylvander`.

### 4.3 First-run layout

Sylvander creates its data directory and the default session/evidence/
memory/profile databases on first successful startup. With
`SYLVANDER_CONFIG` set, the directory comes from the configuration file.
Without it (legacy environment mode), the directory defaults to
`$XDG_DATA_HOME/sylvander` or `~/.local/share/sylvander`.

If you use the integrity anchor in production, pre-create the anchor
directory with `0700` permissions owned by the service account before
starting the server for the first time — the server refuses to start when
the anchor parent is missing or writable by the database writer.

## 5. Quickstart

The shortest path from a clean checkout to a working session:

```sh
# 1. Configure (start from the maintained example)
cp config/sylvander.example.toml /etc/sylvander/server.toml
$EDITOR /etc/sylvander/server.toml

# 2. Provide required secrets
export ANTHROPIC_API_KEY=sk-ant-...
export ANTHROPIC_BASE_URL=https://api.anthropic.com
export SYLVANDER_MODEL=claude-sonnet-4-7

# 3. Run
export SYLVANDER_CONFIG=/etc/sylvander/server.toml
./target/release/sylvander
```

The server prints a structured startup line such as:

```
INFO sylvander_server: server configuration loaded path=/etc/sylvander/server.toml
INFO sylvander_server: channel configured instance=terminal kind=unix
INFO sylvander_server: sylvander server running server="sylvander" agents=1 channels=1
```

Connect the TUI:

```sh
# Build the TUI client once
cargo build --release -p sylvander-tui

# Connect using the default Unix socket
./target/release/sylvander-tui --socket /tmp/sylvander.sock
```

The first chat creates a new session; subsequent chats reuse it or open
a new one depending on the chosen session binding. Type `/help` in the
TUI to see the available slash commands; the most useful are `/sessions`,
`/model`, `/permissions`, and `/extensions`.

## 6. Configuration

Sylvander's production server is configured by one versioned TOML
document. Set `SYLVANDER_CONFIG` to its path before launching:

```sh
export SYLVANDER_CONFIG=/etc/sylvander/server.toml
sylvander
```

The maintained, fully-commented example lives at
[`config/sylvander.example.toml`](../config/sylvander.example.toml).
Treat that file as **normative** for the public schema — anything not
present in it is not part of the supported surface.

When `SYLVANDER_CONFIG` is unset, the server converts the legacy
environment contract into the same in-memory schema. This is a bounded,
explicitly-approved migration path; **new deployments should use TOML**.

For the full schema reference, validation rules, secret-reference
format, integrity-anchor options, and agent/workspace semantics, see
[`server-configuration.md`](server-configuration.md).

### 6.1 Top-level shape

The schema version is mandatory and must equal `1`. The example contains
six top-level tables:

- `[server]` — name, mode, data directory, session/evidence/memory/
  profile databases, boundary quotas, and optional sub-sections for
  memory maintenance, approval, evidence, and stable identity.
- `[[model_providers]]` — Anthropic-compatible (or other) provider
  definitions with credential references and per-model capability
  catalogs.
- `[[execution_targets]]` — `local`, `ssh`, `container`, or `sandbox`
  backends with bounded resource ceilings.
- `[[agents]]` — versioned Agent definitions: persona, model,
  permissions, workspace mounts, prompt profiles.
- `[[channels]]` — one entry per transport instance, with supervision
  policy and credential references.

### 6.2 Secrets

Credentials are **never** embedded as TOML literals. Reference them
through the typed `SecretRef` envelope:

```toml
[model_providers.api_key]
source = "env"
name = "ANTHROPIC_API_KEY"

# or
[model_providers.api_key]
source = "file"
path = "/run/secrets/provider-api-key"
```

Secret files must be regular files no larger than 64 KiB. Resolved values
are redacted from `Debug` formatting and cleared from their temporary
buffer after the client is constructed.

### 6.3 Boundary quotas

`[server.boundary]` controls the global ingress limits applied to every
channel. The defaults are:

```toml
[server.boundary]
max_request_bytes = 1048576   # 1 MiB
requests_per_minute = 240
```

The rate window is isolated by channel instance and authenticated
principal; unauthenticated failures share a bounded anonymous window.

## 7. Modes

The server recognises three operating modes declared in `[server].mode`:

- `production` (default) — fail-closed startup, latest-only schema
  validation, integrity anchor required, durable session/evidence/memory
  stores, supervised channels. This is the only mode with full
  operational guarantees.
- `foreground` — same as `production`, but signal handling and shutdown
  semantics are tuned for interactive sessions: logs are line-buffered,
  the final maintenance state is published synchronously, and an
  unhandled error in a channel task is surfaced immediately rather than
  triggering a supervised restart. Use this for development on a laptop.
- `supervised` — designed to run under an external supervisor
  (systemd, launchd, runit). Startup is identical to `production`; the
  server expects the supervisor to handle respawn, log rotation, and
  watchdog. The server still exits non-zero on any owned component that
  fails to drain cleanly so the supervisor can record the failure.

`SYLVANDER_MODE=self_use` is an additional legacy compatibility value
used by the local-dev `sylvander.env`; it disables the integrity anchor
and the supervised multi-instance contract. Use it only for evaluation.

## 8. Anthropic setup

Sylvander talks to any Anthropic-compatible upstream that accepts the
`/v1/messages` POST format with `Authorization: Bearer …`. Three
environment variables are required at startup:

| Variable             | Required | Purpose                                                |
|----------------------|:--------:|--------------------------------------------------------|
| `ANTHROPIC_API_KEY`  | yes      | Bearer token sent in the request `Authorization` header |
| `ANTHROPIC_BASE_URL` | yes      | Root URL of the upstream (no trailing slash)            |
| `SYLVANDER_MODEL`    | yes      | Default model id the gateway must recognise             |

`SYLVANDER_MODEL` is intentionally required rather than optional: a typo
or unknown id surfaces as a startup failure instead of a 404 storm at
runtime (see [`server-env.md`](server-env.md) for the rationale).

### 8.1 Optional model list

| Variable                       | Default              | Purpose                                                                |
|--------------------------------|----------------------|------------------------------------------------------------------------|
| `SYLVANDER_MODELS`             | primary model only   | Comma-separated model ids exposed through `/model`                     |
| `SYLVANDER_REASONING_MODELS`   | empty                | Subset that advertises low/medium/high reasoning; others report `off`  |
| `SYLVANDER_DEPRECATED_MODELS`  | empty                | `model` or `model=replacement`; old sessions may still select them     |
| `SYLVANDER_MODEL_PRICING`      | empty                | `model=input:output[:cache_write:cache_read]` USD per million tokens   |

Invalid values in any of these variables fail startup; omitted prices
remain explicitly unknown rather than defaulting to zero.

### 8.2 Credential rotation

The Provider credentials bound to an Agent rotate **live** by generation.
Rotation is an explicit, optimistic, content-free UI protocol operation:

1. the operator stages a new generation through `CreateCredentialDraft`;
2. preflight validates the new credential against the active Provider;
3. `ActivateCredential` performs the SQL CAS;
4. the new generation is used for all *new* Provider requests; in-flight
   requests continue with the generation they started under;
5. the previous generation stays reachable for historical audit reads
   but is no longer selected for new sessions.

Rotation does **not** require restarting the server, and resolved secret
values are never persisted in the session or evidence databases.

### 8.3 Examples

Official Anthropic API:

```sh
export ANTHROPIC_API_KEY=sk-ant-...
export ANTHROPIC_BASE_URL=https://api.anthropic.com
export SYLVANDER_MODEL=claude-sonnet-4-7
./target/release/sylvander
```

Internal compatible gateway with a deprecated-model migration:

```sh
export ANTHROPIC_API_KEY=anything-here
export ANTHROPIC_BASE_URL=http://my-gateway.internal:9527
export SYLVANDER_MODEL=fast-code
export SYLVANDER_MODELS=fast-code,deep-code
export SYLVANDER_REASONING_MODELS=deep-code
export SYLVANDER_DEPRECATED_MODELS=fast-code=deep-code
export SYLVANDER_MODEL_PRICING="fast-code=0.10:0.40,deep-code=3:15:3.75:0.30"
./target/release/sylvander
```

Local mock for development (see `sylvander.env` in the repository root):

```sh
export ANTHROPIC_API_KEY=dev
export ANTHROPIC_BASE_URL=http://127.0.0.1:9527
export SYLVANDER_MODEL=mock-fast
RUST_LOG=debug ./target/release/sylvander
```