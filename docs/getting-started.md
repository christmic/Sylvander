# Start using Sylvander

> The shortest supported path from a checkout to a local Agent session.
> This is the canonical onboarding guide. Detailed reference material is
> linked only when the default path no longer applies.

## 1. Choose the product surface

Sylvander is one server product with several clients and optional adapters:

| Component | Use it when | Owns |
|---|---|---|
| `sylvander` | always | Agents, sessions, models, memory, tools, workspaces, evidence |
| `sylvander-tui` | working in any terminal or over SSH | one focused Agent session |
| `Sylvander.app` | working locally on macOS with several sessions | native session rail, retained Ghostty PTYs, Inspectors |
| Unix/HTTP/WS channels | integrating another local or web client | transport into the same public UI protocol |
| DingTalk/Telegram/WeChat channels | reaching an Agent from chat | independently configured bot instances |

Start the server first. Choose either the TUI or the macOS app as the client.

## 2. Local self-use in ten minutes

### 2.1 Build

From the repository root:

```sh
cargo build --release --locked -p sylvander-server -p sylvander-tui
```

Artifacts:

```text
target/release/sylvander       server
target/release/sylvander-tui   terminal client
```

Both binaries are self-describing:

```sh
target/release/sylvander --help
target/release/sylvander-tui --help
```

Use `--version` to report the package version. The server intentionally reads
its configuration path from `SYLVANDER_CONFIG`; the TUI help lists its three
launch options and points to the separate preference reference.

### 2.2 Create the local configuration

Copy the maintained self-use profile:

```sh
mkdir -p "$HOME/.config/sylvander" /tmp/sylvander-agent
cp config/sylvander.local.example.toml \
  "$HOME/.config/sylvander/server.toml"
```

Open `server.toml` and edit only what your Provider requires:

1. `model_providers.base_url`;
2. `model_providers.models.id`;
3. `agents.spec.model.model_name`;
4. the matching entries in `allowed_models` and `qualified_models`.

The default Agent workspace is `/tmp/sylvander-agent`. Replace it with an
absolute, writable directory if you want durable Agent-home instructions,
Skills, or MCP definitions.

The local profile intentionally uses:

- `server.mode = "self_use"`;
- metadata-only evidence;
- no external memory-integrity anchor;
- one local execution target;
- one Agent;
- one Unix channel at `/tmp/sylvander.sock`.

The production example is
[`config/sylvander.example.toml`](../config/sylvander.example.toml). It is not
the recommended first-run file.

### 2.3 Provide the configured secret

The local example names one environment-backed secret:

```sh
export ANTHROPIC_API_KEY='your-provider-key'
```

If you changed `[model_providers.api_key].name`, export that name instead.
Sylvander does not know or guess Provider-specific secret names.

### 2.4 Start the server

Terminal 1:

```sh
export SYLVANDER_CONFIG="$HOME/.config/sylvander/server.toml"
./target/release/sylvander
```

Healthy startup includes these facts:

```text
server configuration loaded
channel configured instance=terminal kind=unix
sylvander server running
```

The server stays in the foreground. Stop it with `Ctrl+C`.

### 2.5 Start one client

Terminal 2, standalone TUI:

```sh
./target/release/sylvander-tui --socket /tmp/sylvander.sock
```

Or launch the built macOS app. It uses the same socket by default and starts
the packaged TUI inside each retained Ghostty surface.

Do not pass `/tmp/sylvander.sock` as a bare positional argument. The current
TUI interface uses `--socket`.

## 3. What to expect

On a healthy connection:

- the TUI displays the Seed-Crab Welcome block;
- the bottom Composer has focus and shows a cursor;
- sending the first message creates or binds a session;
- the bottom status row reports the effective model, branch, session, tokens,
  and tool activity;
- tool approvals and Agent questions replace the Composer in the bottom
  decision surface;
- keyboard transcript navigation reviews history while the mouse wheel keeps
  native terminal scrolling behavior.

In the macOS app:

- the left rail lists server sessions;
- selecting a session switches to its retained PTY;
- the terminal still contains exactly one single-session TUI;
- changes and previews open in the right Inspector;
- `⌘B` toggles the session rail.

## 4. Configuration: one source per concern

### 4.1 Server settings

The server reads exactly one settings entry point:

| Variable | Default | Purpose |
|---|---|---|
| `SYLVANDER_CONFIG` | none; required | current schema-v1 TOML path |
| `RUST_LOG` | `info` | standard tracing filter |
| `SYLVANDER_LOG_FORMAT` | human | set to `json` for JSON logs |

Agents, models, workspaces, channels, permissions, execution targets, prompts,
memory, and limits belong in the TOML document. They are not environment
overrides.

See [`server-configuration.md`](server-configuration.md) for every TOML field
and [`server-env.md`](server-env.md) for the strict environment contract.

### 4.2 Secrets referenced by the server

A TOML object may name an environment or file secret:

```toml
[model_providers.api_key]
source = "env"
name = "ANTHROPIC_API_KEY"
```

That variable is required because this configuration names it, not because it
is a global Sylvander switch. Production memory, evidence, channel, identity,
or SSH secrets follow the same `SecretRef` contract.

### 4.3 TUI preferences

The TUI works without any preference variables. Its only commonly needed
launch option is:

```sh
sylvander-tui --socket /tmp/sylvander.sock
```

Optional settings are grouped under `SYLVANDER_TUI_*`:

- appearance: `THEME`, `FOREGROUND`, `ACCENT`, `COLOR`;
- editing/accessibility: `EDITING`, `REDUCED_MOTION`, `NO_ITALIC`;
- responsiveness: `RENDER_FPS`, `ANIMATION_MS`, `RECONNECT_MS`;
- input/navigation: `MOUSE_SCROLL_LINES`, `KEY_*`;
- local persistence: `SYLVANDER_HISTORY_PATH`.

Do not set them until the default behavior has a concrete problem. The
authoritative names, defaults, ranges, and precedence are in
[`../sylvander-tui/docs/CONFIGURATION.md`](../sylvander-tui/docs/CONFIGURATION.md).
The TUI `/config` view shows the resolved runtime values.

### 4.4 macOS desktop host

`Sylvander.app` normally needs no additional settings:

| Variable | Default | Purpose |
|---|---|---|
| `SYLVANDER_SOCKET` | `/tmp/sylvander.sock` | server Unix socket |

Host-bridge token variables are process-private values generated by the app
for its packaged TUI. They are not user configuration.

## 5. Agent, model, and workspace precedence

The effective session is resolved in this order:

```text
session override
    ↓
Agent default
    ↓
server configuration
```

- An Agent owns its identity, persona, memory, default model, allowed model
  set, prompt profile, and Agent workspace.
- A session may override only values explicitly allowed by the Agent,
  including its model and task workspace.
- The Agent workspace is its durable home. The task workspace is the project
  the current session operates on.
- A workspace may be local, SSH-backed, container-backed, or sandbox-backed.
  Tools use the execution abstraction and do not need to know where it lives.
- Writable Git task workspaces use isolated worktrees when the configured
  coding workflow requests one; merge remains a separate reviewable action.

## 6. Moving from local to production

Use [`config/sylvander.example.toml`](../config/sylvander.example.toml) only
after local self-use works. Production additionally requires:

- a durable private `server.data_dir`;
- evidence encryption;
- memory integrity key plus an independent file or HTTP anchor;
- explicit boundary authentication and Agent access policy;
- backed-up databases and recovery drills;
- channel credentials through environment or mounted files;
- release verification for every client you distribute.

Follow [`operations-runbook.md`](operations-runbook.md) and
[`release-closure.md`](release-closure.md); do not promote the self-use profile
by changing only `server.mode`.

## 7. Fast diagnosis

### Server says `SYLVANDER_CONFIG must name...`

Export `SYLVANDER_CONFIG` in the same shell that starts the server.

### Configuration names an unavailable secret

Export the exact variable named by its `SecretRef`, or change the reference to
a readable secret file.

### TUI says it cannot connect

Confirm the server is still running and the configured Unix channel path
matches `--socket`:

```sh
ls -l /tmp/sylvander.sock
```

### Desktop session cannot start

The session workspace must be an existing absolute directory from the desktop
host's point of view. Fix the session/Agent workspace and restart the terminal
surface.

### Colors or transparency are wrong

Run `/config` in the TUI. Outside the desktop host, check `TERM`,
`COLORTERM`, `NO_COLOR`, and any explicit `SYLVANDER_TUI_COLOR`. Inside the
desktop host, follow

## 8. Where to go next

- Daily use: [`user-manual.md`](user-manual.md)
- Server schema: [`server-configuration.md`](server-configuration.md)
- TUI interaction: [`sylvander-tui-ux-design.md`](sylvander-tui-ux-design.md)
- Desktop interaction: [`sylvander-desktop-ux-design.md`](sylvander-desktop-ux-design.md)
- Channel operations: [`chat-channel-operations.md`](chat-channel-operations.md)
- Development: [`developer-manual.md`](developer-manual.md)
