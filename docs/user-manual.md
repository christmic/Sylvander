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
## 9. Channels overview

A **channel** is one ingress adapter instance. Every `[[channels]]`
entry binds a stable instance id, a transport kind, a default Agent, a
supervision policy, and secret references. The supported kinds are
`unix`, `http`, `websocket`, `dingtalk`, `telegram`, and `wechat` —
each documented in §10–§15. All share the same authenticated,
default-deny Agent access policy, instance-scoped bus subscriptions,
replay suppression, bounded restart/backoff, and cooperative drain.
See [`channel-supervision.md`](../sylvander-runtime/docs/channel-supervision.md)
and [`boundary-authorization.md`](boundary-authorization.md).

## 10. Unix socket quickstart

```toml
[[channels]]
id = "terminal"
enabled = true
default_agent = "sylvander"

[channels.transport]
kind = "unix"
path = "/tmp/sylvander.sock"
```

Connect the TUI:

```sh
cargo build --release -p sylvander-tui
./target/release/sylvander-tui --socket /tmp/sylvander.sock
```

Protocol: one JSON object per line (NDJSON). Client → server messages
include `{"type":"chat","text":"hi"}`,
`{"type":"approve","call_id":"...","approved":true}`,
`{"type":"list_sessions"}`, and `{"type":"ping"}`. Server → client
pushed events include `text_delta`, `tool_call`, `tool_result`,
`tool_rejected`, `approval_request`, `iteration_start`, `done`,
`error`, `session_created`, and `pong`.

## 11. HTTP quickstart

The HTTP channel streams responses as Server-Sent Events:

```toml
[[channels]]
id = "http-debug"
enabled = false
default_agent = "sylvander"

[channels.transport]
kind = "http"
bind = "127.0.0.1:8080"
principal_id = "local-http-client"

[channels.transport.bearer_token]
source = "env"
name = "SYLVANDER_HTTP_TOKEN"
```

```sh
curl -N -X POST http://127.0.0.1:8080/chat \
  -H "Authorization: Bearer ${SYLVANDER_HTTP_TOKEN}" \
  -H 'Content-Type: application/json' \
  -d '{"session_id":"demo","message":"hello"}'
```

`session_id` may be omitted; the server returns a `session_created`
event with the assigned id. The HTTP surface is intentionally narrower
than Unix/WebSocket — it accepts authenticated chat and decision
answers, exposes `/health`, `/ready`, `/metrics`, but does **not**
expose Agent administration or profile editing.

## 12. WebSocket quickstart

The WebSocket channel exposes the complete typed UI protocol,
including Agent administration, User Profile, and Identity Binding
when those capabilities are negotiated. Config:

```toml
[[channels]]
id = "ws-desktop"
enabled = true
default_agent = "sylvander"

[channels.transport]
kind = "websocket"
bind = "127.0.0.1:8081"
principal_id = "ws-desktop"

[channels.transport.bearer_token]
source = "env"
name = "SYLVANDER_WS_TOKEN"
```

Minimal client:

```js
const ws = new WebSocket("ws://127.0.0.1:8081", {
  headers: { Authorization: `Bearer ${token}` }
});
ws.onmessage = (event) => { /* text_delta, tool_call, done, ... */ };
ws.onopen = () => ws.send(JSON.stringify({ type: "chat", text: "hi" }));
```

## 13. Telegram setup

```toml
[[channels]]
id = "telegram-primary"
enabled = true
default_agent = "sylvander"

[channels.transport]
kind = "telegram"
bind = "127.0.0.1:8090"

[channels.transport.token]
source = "env"
name = "TELEGRAM_PRIMARY_TOKEN"

[channels.transport.webhook_secret]
source = "env"
name = "TELEGRAM_PRIMARY_WEBHOOK_SECRET"
```

Provision the webhook:

```sh
curl -X POST "https://api.telegram.org/bot${TELEGRAM_PRIMARY_TOKEN}/setWebhook" \
  --data-urlencode "url=https://your-host/telegram/webhook" \
  --data-urlencode "secret_token=${TELEGRAM_PRIMARY_WEBHOOK_SECRET}"
```

The webhook secret must match the `X-Telegram-Bot-Api-Secret-Token`
header on every request.

## 14. DingTalk setup

```toml
[[channels]]
id = "dingtalk-primary"
enabled = true
default_agent = "sylvander"

[channels.transport]
kind = "dingtalk"

[channels.transport.app_key]
source = "env"
name = "DINGTALK_APP_KEY"

[channels.transport.app_secret]
source = "env"
name = "DINGTALK_APP_SECRET"
```

The channel is enabled when **both** `app_key` and `app_secret` are
present. Sessions, principals, replay protection, interactive
decisions, and bounded delivery retry are all instance-scoped; one bot
failing does not affect others.

## 15. WeChat setup

```toml
[[channels]]
id = "wechat-primary"
enabled = true
default_agent = "sylvander"

[channels.transport]
kind = "wechat"
bind = "127.0.0.1:8091"

[channels.transport.corp_id]
source = "env"
name = "WECHAT_CORP_ID"

[channels.transport.secret]
source = "env"
name = "WECHAT_SECRET"

[channels.transport.token]
source = "env"
name = "WECHAT_TOKEN"

[channels.transport.encoding_aes_key]
source = "env"
name = "WECHAT_ENCODING_AES_KEY"
```

Credentials are resolved at startup so the server fails fast before
accepting traffic. The WeChat surface is intentionally narrower than
Unix/WebSocket.

## 16. Approval / AskUser flow

Some Agent actions require an explicit per-session approval. The Agent
emits an `approval_request` (or `tool_approval_required`) event with a
`batch_id` and the list of tools in the batch. The connected client
responds with
`{"type":"approve","call_id":"<batch_id>","approved":true|false}`.
Approvals match one batch and do not mutate the Agent or the
persistent policy.

Persistent approvals across restarts require both:

```sh
export SYLVANDER_APPROVAL=1
export SYLVANDER_APPROVAL_STORE=/var/lib/sylvander/approvals.json
```

Without the store, the approval gate is session-scoped only.

## 17. User Profile

Sylvander owns one global User Profile per stable `UserId`, addressed
through the public `user_profile_v1` capability. Unix and WebSocket
clients negotiate the capability at hello time; HTTP and external
chat channels do not currently expose the editing surface.

The profile contains preferred language and locale; response detail
(`concise`, `balanced`, `detailed`); communication tone (`direct`,
`warm`, `formal`); accessibility preferences (screen-reader,
reduced-motion, high-contrast); and at most 16 bounded user-owned
interaction constraints. Each preference carries a `PrivacyClass`
(`personal`, `sensitive`, `restricted`) — class is policy input, and
Runtime enforces it; profile data and exports redact their `Debug`
output regardless of class.

Operations are `create`, `read`, `update`, `export`, `correct`,
`delete`, and `set_do_not_learn`. Mutations require an optimistic
`expected_revision` and fail with a typed conflict on staleness. The
wire contract is in [`user-profile-protocol.md`](user-profile-protocol.md);
storage placement, backup, and retention are in
[`server-configuration.md`](server-configuration.md#global-user-profile).

`do_not_learn = true` prohibits creating new learned profile facts,
Relationship Memory observations, Agent private candidates derived
from the user, or cross-user canonical memory derived from the user.
Deletion preserves the durable opt-out as a tombstone; re-creating a
profile inherits that marker until the owner changes it explicitly.
The TUI negotiates `user_profile_v1` but does not yet expose an
editing surface — use a protocol client until the TUI editor lands.

## 18. Identity binding

Stable identity binding links an authenticated external principal
(Telegram user, DingTalk staff id, Unix peer credential) to a Sylvander
`UserId`. The capability is `identity_binding_v1`; both peers must
advertise it before any binding operation succeeds. Operations are
`begin`, `confirm`, `resolve`, and `unlink`. Begin is initiated by a
stable user (Unix/WebSocket); the external principal completes the
link with `confirm` from the chat platform. The two-sided proof
prevents an external account from claiming a known `UserId`.

Trust boundary:

```text
authenticated transport ingress
  -> BoundaryContext established by that transport
  -> ChannelContext derives AuthenticatedTransportIdentity
  -> Runtime UiService re-authorizes boundary + typed identity
  -> Runtime-owned PrincipalBindingStore
```

Production Runtime enables the capability only when
`server.identity.digest_key` and at least one `trusted_issuers` entry
are configured. The store keeps only HMAC-keyed digests of external
principal ids, revisions, and bounded challenge state; raw principal
ids and link secrets are never persisted. See
[`identity-binding-protocol.md`](identity-binding-protocol.md).

## 19. Run evidence

Every server-process lifetime produces one durable **run** in the
evidence ledger. A run contains turns, steps, decisions, outcomes,
and optional feedback. The ledger is the authoritative source for
triage, evaluation, and the gated self-improvement pipeline — it is
not a log archive and it does not authorise an Agent to modify or
deploy itself.

The recorder subscribes to the bus **before** configured Agents
start, so no event is missed. On graceful shutdown it drains queued
messages, marks active turns as `interrupted`, then closes the run.
On restart, any remaining open run/turn/step is marked
`interrupted`; evidence never converts an unknown result into success.

Capture policies:

| Policy         | What is stored                                              |
|----------------|-------------------------------------------------------------|
| `metadata_only`| Event types, timestamps, byte sizes, attachment counts, digests |
| `redacted`     | Adds a structural envelope with payload replaced by `[REDACTED]` |
| `full`         | Stores the serialized bus message; opt-in only              |

`server.evidence.content = "metadata_only"` is the default and the
recommended production setting. `full` requires an operator-defined
privacy, access, backup, and deletion policy. Retention defaults to
30 days; completed runs older than `retention_days` are deleted at
startup. Active and crash-recovery records are retained.

Query APIs return bounded turn summaries with step and failure
counts; raw payloads are not part of those summaries. Cohort analysis
requires an explicit half-open time window and bounded result limit;
reports expose success rate, failure taxonomy, token usage, latency,
tool activity, and feedback coverage — never raw prompt, response,
tool payload, or memory content. See
[`runtime-evidence.md`](runtime-evidence.md).

## 20. TUI usage, health, logging, shutdown, FAQ

### 20.1 TUI usage

The TUI client (`sylvander-tui`) connects over the Unix socket
channel. Useful slash commands: `/help`, `/sessions`, `/model`,
`/permissions`, `/extensions`, `/hooks`, `/identity`, `/quit`.
Session-level overrides live with the session, not the Agent.

### 20.2 Health, readiness, metrics

An enabled HTTP channel exposes three unauthenticated, content-free
operations:

```sh
curl --fail http://127.0.0.1:8080/ready
curl --fail http://127.0.0.1:8080/health
curl --fail http://127.0.0.1:8080/metrics
```

- `GET /health` returns the Runtime dependency snapshot (200/503).
- `GET /ready` returns `{"ready":true|false}` (200/503).
- `GET /metrics` returns Prometheus text for Agent/session/channel
  counts, bounded message-bus capacity/subscribers, successful
  publishes, and backpressure rejections.

The snapshot never contains prompts, messages, tool inputs, external
principal ids, credentials, paths, or memory content. Readiness must
not be replaced with process liveness.

### 20.3 Logging

`RUST_LOG` selects the standard tracing-subscriber filter. Set
`SYLVANDER_LOG_FORMAT=json` for one flattened JSON object per event:

```sh
RUST_LOG=sylvander_runtime=info,sylvander_agent=info \
SYLVANDER_LOG_FORMAT=json \
./target/release/sylvander
```

Logs are operational events, not transcript export. Credentials and
raw provider secrets must never appear as fields.

### 20.4 Shutdown

Send `SIGINT` **once** to begin graceful shutdown. The Runtime stops
accepting channel work, cooperatively drains channel tasks, stops
Agent workers, closes active evidence turns, publishes the final
maintenance state, and returns an error if any owned component
failed to drain. Do not send a second signal unless the configured
external supervisor has exceeded its termination deadline.

### 20.5 FAQ / troubleshooting

- **Startup fails with `ANTHROPIC_API_KEY must be set`.** Export it
  in the same shell, or use a TOML `SecretRef` pointing to a file
  containing the key.
- **`ANTHROPIC_BASE_URL is set but empty`.** The variable is exported
  but contains the empty string.
- **`Anthropic API error (status 404): unknown: (no message)`.** The
  gateway does not recognise `SYLVANDER_MODEL`. Check the id against
  `/v1/models`.
- **TUI connects but every chat returns `session_id required`.** Send
  the first chat without `session_id`; the server returns
  `session_created`.
- **`/metrics` shows increasing
  `sylvander_bus_backpressure_rejections_total`.** A subscription is
  full. Identify the stalled channel or client from the structured
  logs (use the stable `instance` field) before retrying submissions.
- **Telegram bot stops receiving updates after a redeploy.** The
  webhook secret changed or `setWebhook` was not reissued.
- **DingTalk instance stops responding after a key rotation.** Update
  the `SecretRef`, restart the channel; the new generation is used
  for new sessions.
- **HTTP channel returns 401 to a previously-working client.** The
  bearer token was rotated; re-export the new value.
- **Memory startup fails after a host migration.** Do **not** delete
  or rewrite the database. Verify the configured integrity anchor is
  reachable and follow the signed backup/restore procedure in
  [`operations-runbook.md`](operations-runbook.md#incident-triage).
- **Coding session lost edits after a SIGKILL.** The session
  workspace is a worktree lease. Restart the server — boot validates
  active leases and recovers worktrees left before manifest commit.
- **TUI shows `user_profile_v1` but no editing controls.** Expected.
  Use a Unix/WebSocket protocol client until the TUI editor lands.
- **Bug reports.** Open an issue; include the server version, the
  structured log line, the affected channel instance id, and (if
  reproducible) a redacted turn summary from the evidence query API.
