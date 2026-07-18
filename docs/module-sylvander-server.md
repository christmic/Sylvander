# Module Reference — `sylvander-server`

> Composition root for the Sylvander server binary.
> Source: [`sylvander-server/src/main.rs`](../sylvander-server/src/main.rs)

## 1. Purpose

`sylvander-server` is the **composition root** that wires Sylvander's runtime,
agents, providers, and channels into one runnable binary. It is intentionally
minimal: a single `main.rs` that performs tracing init, loads server config,
boots the runtime, builds channels, starts them, and supervises shutdown.
No CLI flags, no interactive prompts, no embedded web UI. The developer
manual is the operator's entry point.

## 2. Public surface

The binary is **not a library**: there is no public Rust API. Operators
configure it via environment variables and a `ServerConfig` file. The
relevant types from `sylvander-runtime` are re-used internally:

```rust
// Re-exported via sylvander_runtime::config
pub struct ServerConfig { /* see sylvander-runtime::config */ }
pub enum ChannelTransportConfig {
    Unix    { path: PathBuf },
    Http    { bind: String, principal_id: String, bearer_token: SecretRef },
    Websocket { bind: String, principal_id: String, bearer_token: SecretRef },
    DingTalk  { app_key: SecretRef, app_secret: SecretRef },
    Telegram  { token: SecretRef, bind: String, webhook_secret: SecretRef },
    Wechat    { bind: String, corp_id: String, agent_id: String,
                secret: SecretRef, token: SecretRef,
                encoding_aes_key: SecretRef },
}

// Public transport-neutral contract in sylvander-channel:
pub trait CredentialLeaseSource { async fn lease(&self, request: &CredentialLeaseRequest) -> ...; }

// Internal server adapter:
struct SystemChannelCredentialSource { /* SecretRef map + resolver + generation state */ }

// Internal enum (sylvander-server/src/main.rs):
enum ServerError {
    MissingConfig,
    Config(ConfigError),
    Runtime(RuntimeError),
    UnknownAgent(String),
    UnknownModel(String),
    Channel { id: String, message: String },
    Address { value: String, message: String },
    Signal(String),
    ChannelStopped(String),
    AgentStopped(String),
}
```

`ChannelTransportConfig` variants and their renewable credential slots:

| Variant | Lease slots | Operation boundary |
|---------|-------------|--------------------|
| `Unix` | none | n/a |
| `Http` | `bearer_token` | each chat authentication |
| `Websocket` | `bearer_token` | each HTTP upgrade |
| `DingTalk` | `app_key`, `app_secret` | Stream connection and access-token refresh |
| `Telegram` | `bot_token`, `webhook_secret` | Bot API delivery and webhook authentication |
| `Wechat` | `api_secret`, `callback_token`, `encoding_aes_key` | API token refresh and callback codec creation |

The composition root passes only the source and stable channel instance to an
adapter. `SystemChannelCredentialSource` resolves all requested slots together,
publishes a new credential generation only after complete success, and issues
a bounded lease for each operation. It re-reads file/environment references,
so rotation does not require rebuilding the channel.

## 3. Architecture

```text
                    +------------------------+
                    | main()                 |
                    | init_tracing           |
                    | load_config            |
                    +-----------+------------+
                                |
                                v
                    +------------------------+
                    | Runtime::boot_config   |
                    | (sylvander-runtime)    |
                    +-----------+------------+
                                |
                                v
                    +------------------------+
                    | build_channels         |
                    |   - register secret    |
                    |     lease slots        |
                    |   - construct Arc<dyn  |
                    |     Channel> per kind  |
                    +-----------+------------+
                                |
                                v
                    +------------------------+
                    | runtime.start_channels |
                    +-----------+------------+
                                |
                                v
                    +------------------------+
                    | tokio::select!         |
                    |  - ctrl_c              |
                    |  - wait_for_channel_   |
                    |    exit                |
                    |  - wait_for_agent_exit |
                    +-----------+------------+
                                |
                                v
                    +------------------------+
                    | runtime.shutdown()     |
                    +------------------------+
```

## 4. Lifecycle / data flow

The verified code path (`sylvander-server/src/main.rs:19`) is:

1. `init_tracing` — installs a `tracing_subscriber::fmt` layer with the
   `EnvFilter` from `RUST_LOG`, switching to JSON when
   `SYLVANDER_LOG_FORMAT=json`.
2. `load_config` — the current product contract requires
   `SYLVANDER_CONFIG` to identify the latest-version configuration file.
   Missing, empty, non-Unicode, unreadable, old, or unknown configuration
   fails before Runtime boot; the binary has no environment-only conversion.
3. `Runtime::boot_config(config.clone())` — boots the runtime with the
   resolved `ServerConfig`.
4. `build_channels(&config, &runtime)` — iterates enabled channels,
   maps each channel's secret references to an instance-scoped renewable
   lease source, and constructs the matching `Arc<dyn Channel>` from the per-transport
   crates (`sylvander_channel_unix`, `sylvander_channel_http`,
   `sylvander_channel_ws`, `sylvander_channel_dingtalk`,
   `sylvander_channel_telegram`, `sylvander_channel_wechat`). Each
   registration receives session defaults (e.g. the channel's
   `default_workspace`) and a `ChannelRestartPolicy` with
   `max_attempts`, `initial_backoff`, and `max_backoff` from the
   config.
5. `runtime.start_channels(channels).await` — moves channels into the
   runtime's supervisor loop. The info log records `server`,
   `agents`, and `channels`.
6. `tokio::select!` — race `ctrl_c`, `runtime.wait_for_channel_exit`,
   and `runtime.wait_for_agent_exit`. Whichever branch completes
   first is captured as a `RuntimeFailure` (or `None` on a clean
   `ctrl_c`).
7. `runtime.shutdown().await` — orderly drain. A shutdown error
   combined with a runtime failure is logged and the failure is
   returned; otherwise the shutdown result is returned.

## 5. Configuration knobs

| Knob | Type | Effect |
|------|------|--------|
| `SYLVANDER_CONFIG` | path | Required current contract: loads the versioned `ServerConfig` file |
| `SYLVANDER_LOG_FORMAT` | `json` | Switches the tracing subscriber to JSON output |
| `RUST_LOG` | env-filter | Standard `tracing_subscriber::EnvFilter` directives |
| `config.server.name` | string | Logged as `server` on the running line |
| `config.server.boundary.max_request_bytes` | usize | Propagated to every channel via `.with_request_limit(...)` |
| `config.channels[].enabled` | bool | Disabled channels are filtered out in `build_channels` |
| `config.channels[].supervision.*` | restart policy | `max_restart_attempts`, `initial_backoff_ms`, `max_backoff_ms` |
| `config.channels[].default_workspace` | optional binding | Maps to `SessionConfigOverrides::user_workspace` per session |
| secret refs | `SecretRef` | Resolved atomically at each native operation boundary |

## 6. Composition extension rules

- Add a new channel by extending the latest `ChannelTransportConfig`,
  registering exact credential slots here, and returning one supervised
  registration. Never pass resolved credential strings into a channel
  constructor.
- Provider, store, Agent, identity, and authorization construction belongs in
  Runtime composition, not in this binary.
- Startup must fail before listeners become ready when configuration, durable
  state, or required identities cannot be validated. Renewable secret
  unavailability fails the first dependent operation closed and remains
  recoverable without restart.
- A new wait branch must still converge on the single `Runtime::shutdown`
  path; do not add independent process-lifetime owners.

## 7. Tests

| Surface | Test location | Notes |
|---------|---------------|-------|
| `Runtime::boot_config` | `sylvander-runtime` integration tests | Verifies supervisor behaviour for enabled/disabled channels |
| `Runtime::start_channels` + shutdown | `sylvander-runtime` integration tests | End-to-end channel lifecycle |
| channel credential source | `sylvander-server/tests/unit/credential.rs` | Rotation, atomic partial-failure handling, instance/slot isolation, redacted debug |
| production composition and same-kind instances | `sylvander-server/tests/channel_instances.rs` | Starts the shipped server binary with a real `ServerConfig`; proves two HTTP instances bind independently, reject each other's bearer token, keep separate credential-audit subjects, and drain on `SIGINT` |
| current config entry point | `sylvander-server/tests/unit/server_main.rs` | Missing/empty `SYLVANDER_CONFIG` fails; a present path is preserved |
| `init_tracing` | smoke test via `RUST_LOG` override | Logs are not asserted in unit tests |

The production files contain only test-module path bridges; every test body
lives under `sylvander-server/tests/`. Server behavior is verified through
those white-box tests, runtime/channel integration tests, and the operations
runbook drills. The multi-instance journey is deliberately black-box: it
launches `CARGO_BIN_EXE_sylvander` instead of reproducing `build_channels` in a
test factory, so its lifecycle and credential evidence comes from the same
composition root that operators run.

## 8. Related docs

- [`docs/server-env.md`](server-env.md) — full env-var reference for `SYLVANDER_*`.
- [`docs/server-configuration.md`](server-configuration.md) — `ServerConfig` schema, channel blocks, secret references.
- [`docs/operations-runbook.md`](operations-runbook.md) — startup / shutdown / supervision drills.
- [`docs/recovery-drills.md`](recovery-drills.md) — crash recovery for the supervisor.
- [`docs/runtime-evidence.md`](runtime-evidence.md) — runtime evidence that exercises this composition root.
- [`docs/credential-leases.md`](credential-leases.md) — renewable Provider and channel credentials.
- [`AGENTS.md`](../AGENTS.md) — project-wide agent guide.

Co-Authored-By: 🦀 <oraculo@oraculo.ai>
