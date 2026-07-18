# Sylvander user manual

Status: active user documentation

Last updated: 2026-07-18

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
- a Rust 1.96+ toolchain if you are building from source; prebuilt binaries
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
- network access to every Provider `base_url` selected by the active
  configuration, plus optional access to MCP servers, external chat
  platform webhook URLs, and any non-`local` execution target;
- OCI runtime (`docker`, `podman`, or compatible) only if you intend to
  use the `container` or `sandbox` execution targets; otherwise it is
  unused.

For evaluation and tutorial use, a single macOS or Linux workstation with
no OCI runtime and no integrity anchor is sufficient — the agent-only
self-use mode skips the integrity anchor.

## 4. Installation

### 4.1 macOS application bundle

`Sylvander.app` is the macOS desktop-host distribution target. The repository
can build a local Release bundle with its embedded universal
`sylvander-tui` helper, but a public bundle is installable release evidence
only after Developer ID signing, notarization, and stapling pass. See
[`ghostty-release-verification.md`](ghostty-release-verification.md).

The application does not currently provision the server, write
`server.toml`, or import Provider credentials. Configure and start the
server separately, then launch the app against its Unix socket (the default is
`/tmp/sylvander.sock`; `SYLVANDER_SOCKET` overrides it for the desktop host).
Sparkle code is present in the upstream host, but no public Sylvander update
channel should be assumed until a signed release feed is published.

### 4.2 Standalone daemon from source

Build the server with the workspace's pinned toolchain:

```sh
git clone https://github.com/christmic/Sylvander.git
cd Sylvander
cargo install --path sylvander-server --locked
```

`cargo install` places the `sylvander` binary on your `PATH`
(`~/.cargo/bin/sylvander` by default). The `--locked` flag pins
`Cargo.lock`; remove it only when you intentionally want to update
dependencies.

A development build (faster compile, slower runtime) is available with
`cargo build -p sylvander-server` from the workspace root; its artifact lives
at `target/debug/sylvander`. A release build uses
`cargo build --release -p sylvander-server --locked` and produces
`target/release/sylvander`.

### 4.3 First-run layout

Sylvander creates its data directory and the default session/evidence/
memory/profile databases on first successful startup. `SYLVANDER_CONFIG` is
required; `server.data_dir` controls the directory and defaults to
`$XDG_DATA_HOME/sylvander` or `~/.local/share/sylvander` only when that field is
omitted from the current TOML document.

If you use the integrity anchor in production, pre-create the anchor
directory with `0700` permissions owned by the service account before
starting the server for the first time — the server refuses to start when
the anchor parent is missing or writable by the database writer.

## 5. Quickstart

The shortest path from a clean checkout to a working session:

```sh
# 1. Build the server and TUI from the locked workspace
cargo build --release --locked -p sylvander-server -p sylvander-tui

# 2. Configure (start from the maintained example, then select self_use or
#    provision every production path and integrity prerequisite it names)
mkdir -p "$HOME/.config/sylvander"
cp config/sylvander.example.toml "$HOME/.config/sylvander/server.toml"
$EDITOR "$HOME/.config/sylvander/server.toml"

# 3. Provide every secret referenced by that document
export ANTHROPIC_API_KEY=sk-ant-...
# Production example also references memory/evidence keys:
export SYLVANDER_MEMORY_INTEGRITY_KEY=...
export SYLVANDER_EVIDENCE_ENCRYPTION_KEY=...

# 4. Run
export SYLVANDER_CONFIG="$HOME/.config/sylvander/server.toml"
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

When `SYLVANDER_CONFIG` is missing, empty, invalid, unreadable, or points to an
old/unknown schema, startup fails before any listener opens. There is no
environment-only conversion or implicit provider/model fallback.

For the full schema reference, validation rules, secret-reference
format, integrity-anchor options, and agent/workspace semantics, see
[`server-configuration.md`](server-configuration.md).

### 6.1 Top-level shape

The schema version is mandatory and must equal `1`. The current top-level
shape is that scalar plus five configured objects/collections:

- `schema_version` — exact public configuration version;
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

The current configuration schema recognises two modes declared in
`[server].mode`:

- `self_use` (default) — durable local operation for the owner/developer,
  without requiring an independently administered memory integrity anchor.
- `production` — fail-closed startup with the configured evidence encryption
  and independent memory-integrity boundary.

Mode is a TOML field, not an environment override. Both modes use the same
latest configuration schema, qualified model identity, durable session
configuration, supervised channels, and shutdown contract. `self_use` relaxes
only the explicitly documented production trust prerequisites; it is not a
fallback when production configuration is invalid.

## 8. Anthropic setup

Sylvander talks to any Anthropic-compatible upstream that accepts the
`/v1/messages` POST format. Provider URL, models, capabilities, lifecycle, and
pricing are declared under `[[model_providers]]`; only the secret value may
come from an environment/file `SecretRef`.

```toml
[[model_providers]]
id = "primary"
kind = "anthropic_compatible"
base_url = "https://api.anthropic.com"

[model_providers.api_key]
source = "env"
name = "ANTHROPIC_API_KEY"

[[model_providers.models]]
id = "claude-sonnet"
context_window = 200000
max_output_tokens = 32000
capabilities = ["tool_use", "vision", "prompt_caching"]
```

Agent defaults, prompt profiles, and session overrides reference the qualified
pair `(provider_id, model_id)`. A bare model environment variable or
same-named-model guess is rejected. Unknown models, unavailable revision pins,
and capability mismatches fail before credential resolution or dispatch.

### 8.1 Model catalog

Add one `[[model_providers.models]]` block per model. Lifecycle and replacement,
reasoning/tool/media capabilities, context/output limits, and optional pricing
belong to that immutable definition. Omitting pricing means “unknown”; it never
defaults to zero. The active provider/model revisions become durable session
pins and remain exact across restart.

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

Official Anthropic uses the configuration above and only exports the referenced
secret:

```sh
export ANTHROPIC_API_KEY=sk-ant-...
export SYLVANDER_CONFIG=/etc/sylvander/server.toml
./target/release/sylvander
```

For an internal compatible gateway or local mock, change `base_url` and model
definitions in a separate current-schema TOML file. Do not construct a catalog
from comma-separated environment variables:

```sh
export ANTHROPIC_API_KEY=dev
export SYLVANDER_CONFIG=./config/local-mock.toml
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

Protocol: one JSON object per line (NDJSON). The first client frame must be a
current `hello` version/capability negotiation; business messages sent before
the matching server `welcome` are rejected. After negotiation, client → server
messages include `{"type":"chat","text":"hi"}`,
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

`session_id` is a required client-owned alias on the HTTP adapter. Its first
use creates and binds a durable Sylvander session; later requests with the same
alias resume that binding. The HTTP surface is intentionally narrower than
Unix/WebSocket — it accepts authenticated chat and exposes `/health`, `/ready`,
and `/metrics`, but does **not** expose the full typed decision, Agent
administration, or profile-editing protocol.

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
import WebSocket from "ws";

const ws = new WebSocket("ws://127.0.0.1:8081", {
  headers: { Authorization: `Bearer ${token}` }
});
ws.on("open", () => ws.send(JSON.stringify({
  type: "hello",
  protocol: {
    client_name: "example",
    min_version: 5,
    max_version: 5,
    capabilities: []
  }
})));
ws.on("message", (data) => {
  const message = JSON.parse(data.toString());
  if (message.type === "welcome") {
    ws.send(JSON.stringify({ type: "chat", text: "hi" }));
  }
  // Handle text_delta, tool_call, done, and other negotiated events here.
});
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
corp_id = "ww0123456789abcdef"
agent_id = "1000002"

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

`corp_id` and numeric `agent_id` are public WeChat application identifiers.
The three secret references are exposed to the adapter as renewable,
instance-scoped slots: `api_secret`, `callback_token`, and
`encoding_aes_key`. Startup preflights the callback codec and outbound token
path; every callback or delivery obtains a bounded lease, so file/environment
rotation takes effect without rebuilding the channel.

WeChat verifies and decrypts enterprise callbacks, binds the embedded
recipient to `corp_id`, rejects replays, and routes authenticated chat plus
`/approve`, `/deny`, `/answer`, and `/interrupt` controls through Runtime. It
delivers bounded completed replies and tool/control status through the active
message API. Access tokens are cached only for their credential generation,
refreshed once on WeChat expiry codes, and never exposed in logs.

## 16. Approval / AskUser flow

Some Agent actions require an explicit per-session approval. The Agent
emits an `approval_request` (or `tool_approval_required`) event with a
`batch_id` and the list of tools in the batch. The connected client
responds with
`{"type":"approve","call_id":"<batch_id>","approved":true|false}`.
One-shot approvals match one pending call. Session and persistent choices
create an exact grant; they do not mutate the Agent or the persistent policy.

Persistent approvals across restarts require the current configuration to
enable approval and name a durable store:

```toml
[server.approval]
enabled = true
persistent_store = "/var/lib/sylvander/approvals.json"
```

Without the store, the approval gate offers only one-shot and session scopes.
Persistent scope also requires a Runtime-authenticated stable user identity.
Each durable grant is bound to the stable user, Agent, effective approval
policy revision, frozen capability revision, exact tool operation, and a
content-safe resource fingerprint. A change to any dimension invalidates the
grant and prompts again. Old fingerprint-only stores fail startup under the
latest-only schema policy. See
[`../sylvander-agent/docs/approval.md`](../sylvander-agent/docs/approval.md)
for the exact contract and recovery procedure.

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
The TUI exposes the profile through `/profile`. `show`, `create`, `edit`,
`correct`, `do-not-learn on|off`, `export`, and confirmed `delete` are typed
operations; mutations reload and bind the current server revision before
submission. The editor covers every current preference without raw JSON.

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
| `redacted`     | Encrypts structurally redacted JSON under an exact tenant/user scope |
| `full`         | Encrypts the serialized bus message; opt-in only             |

`server.evidence.content = "metadata_only"` is the default and the
recommended production setting. Production still requires
`server.evidence.encryption` because generated MCP result artifacts
share the governed store. `redacted` and `full` never fall back to the
plaintext event table. Retention defaults to 30 days; completed runs
and expired governed event/artifact ciphertext are removed at startup.
Exact tenant/user-scoped export and deletion are auditable, and
deleted record IDs cannot be reused. Active and crash-recovery metadata
records are retained.

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

- **Startup cannot resolve a Provider credential.** Set the environment
  variable or file named by that Provider's TOML `SecretRef`; do not put the
  secret value in the configuration.
- **Startup rejects a Provider URL or catalog.** Correct the Provider
  `base_url` and its current `[[model_providers.models]]` definitions. An
  empty URL or catalog is invalid.
- **The upstream returns an unknown-model error.** Verify the exact
  `(provider_id, model_id)` selected by the Agent/session against that
  Provider's current catalog and upstream deployment. Bare model names are
  never guessed.
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
- **`/profile` is unavailable.** The connected server did not advertise
  `user_profile_v1`; verify Runtime profile storage and the hello/welcome
  capability set. The TUI intentionally fails closed.
- **A profile edit reports a conflict.** Another client changed the profile.
  The stale draft was not applied; `/profile` reloads current server truth
  before the next edit.
- **Bug reports.** Open an issue; include the server version, the
  structured log line, the affected channel instance id, and (if
  reproducible) a redacted turn summary from the evidence query API.
