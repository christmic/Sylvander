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
    Http    { bind: String, principal_id: Option<String>, bearer_token: SecretRef },
    Websocket { bind: String, principal_id: Option<String>, bearer_token: SecretRef },
    DingTalk  { app_key: SecretRef, app_secret: SecretRef },
    Telegram  { token: SecretRef, bind: String, webhook_secret: SecretRef },
    Wechat    { bind: String, corp_id: String, secret: SecretRef,
                token: SecretRef, encoding_aes_key: SecretRef, .. },
}

pub trait SecretResolver { fn resolve(&self, ref: &SecretRef) -> Result<Secret, _>; }
pub struct SystemSecretResolver;

// Internal enum (sylvander-server/src/main.rs):
enum ServerError {
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

`ChannelTransportConfig` variants and their secret-resolution path
(every `SecretRef` is resolved through `SystemSecretResolver::resolve`
before the channel is constructed so startup fails fast):

| Variant | Secrets | Resolved through |
|---------|---------|------------------|
| `Unix` | none | n/a |
| `Http` | `bearer_token` | `SystemSecretResolver` |
| `Websocket` | `bearer_token` | `SystemSecretResolver` |
| `DingTalk` | `app_key`, `app_secret` | `SystemSecretResolver` |
| `Telegram` | `token`, `webhook_secret` | `SystemSecretResolver` |
| `Wechat` | `secret`, `token`, `encoding_aes_key` | `SystemSecretResolver` (resolves `secret` eagerly to fail before traffic) |

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
                    |   - resolve secrets    |
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
2. `load_config` — reads `SYLVANDER_CONFIG` (file path) when present;
   otherwise falls back to `ServerConfig::from_legacy_env()` for
   legacy environment-only deployments.
3. `Runtime::boot_config(config.clone())` — boots the runtime with the
   resolved `ServerConfig`.
4. `build_channels(&config, &runtime)` — iterates enabled channels,
   resolves each channel's secrets via `SystemSecretResolver`, and
   constructs the matching `Arc<dyn Channel>` from the per-transport
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
| `SYLVANDER_CONFIG` | path | Loads `ServerConfig` from the given file. Unset → `from_legacy_env` |
| `SYLVANDER_LOG_FORMAT` | `json` | Switches the tracing subscriber to JSON output |
| `RUST_LOG` | env-filter | Standard `tracing_subscriber::EnvFilter` directives |
| `config.server.name` | string | Logged as `server` on the running line |
| `config.server.boundary.max_request_bytes` | usize | Propagated to every channel via `.with_request_limit(...)` |
| `config.channels[].enabled` | bool | Disabled channels are filtered out in `build_channels` |
| `config.channels[].supervision.*` | restart policy | `max_restart_attempts`, `initial_backoff_ms`, `max_backoff_ms` |
| `config.channels[].default_workspace` | optional binding | Maps to `SessionConfigOverrides::user_workspace` per session |
| secret refs | `SecretRef` | Every variant has its secrets resolved at startup |

## 6. Tests

| Surface | Test location | Notes |
|---------|---------------|-------|
| `Runtime::boot_config` | `sylvander-runtime` integration tests | Verifies supervisor behaviour for enabled/disabled channels |
| `Runtime::start_channels` + shutdown | `sylvander-runtime` integration tests | End-to-end channel lifecycle |
| `build_channels` paths | exercise via runtime tests; deterministic helpers in `sylvander-server/tests` if present | Channel construction logic depends only on `ServerConfig` |
| `init_tracing` | smoke test via `RUST_LOG` override | Logs are not asserted in unit tests |

The binary itself has no `#[cfg(test)]` block; its behaviour is
verified through the runtime and channel integration tests plus the
operations runbook drills.

## 7. Related docs

- [`docs/server-env.md`](server-env.md) — full env-var reference for `SYLVANDER_*`.
- [`docs/server-configuration.md`](server-configuration.md) — `ServerConfig` schema, channel blocks, secret references.
- [`docs/operations-runbook.md`](operations-runbook.md) — startup / shutdown / supervision drills.
- [`docs/recovery-drills.md`](recovery-drills.md) — crash recovery for the supervisor.
- [`docs/runtime-evidence.md`](runtime-evidence.md) — runtime evidence that exercises this composition root.
- [`AGENTS.md`](../AGENTS.md) — project-wide agent guide.

Co-Authored-By: 🦀 <oraculo@oraculo.ai>