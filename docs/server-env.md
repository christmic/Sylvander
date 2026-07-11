# Server environment

The `sylvander` server reads configuration **exclusively from environment
variables** — nothing is baked into the binary. Required vars fail the
process fast at startup so a misconfiguration surfaces immediately
instead of mid-flight.

## Required

| Variable             | What it sets                                            |
|----------------------|---------------------------------------------------------|
| `ANTHROPIC_API_KEY`  | Bearer token sent in `Authorization: Bearer …` headers. Non-empty string required. |
| `ANTHROPIC_BASE_URL` | Root URL of the upstream `/v1/messages` endpoint. Non-empty URL required. |

## Optional

| Variable             | Default                       | Notes                                          |
|----------------------|-------------------------------|------------------------------------------------|
| `SYLVANDER_MODEL`    | `claude-sonnet-5-20260601`    | Model id; check upstream API for allowed values. |
| `SYLVANDER_SOCKET`   | `/tmp/sylvander.sock`         | Path the TUI client uses for its Unix socket. |
| `HTTP_ADDR`          | `127.0.0.1:8080`              | Address the debug HTTP channel binds (always on). |
| `DINGTALK_APP_KEY`   | —                             | DingTalk channel only enabled when *both* this and `DINGTALK_APP_SECRET` are set. |
| `DINGTALK_APP_SECRET`| —                             | (ditto)                                         |
| `SYLVANDER_APPROVAL` | unset                         | Set to any value to enable tool approval gate.  |
| `RUST_LOG`           | `info`                        | Standard tracing-subscriber filter.            |

## Examples

### Official Anthropic API

```sh
export ANTHROPIC_API_KEY=sk-ant-...
export ANTHROPIC_BASE_URL=https://api.anthropic.com
./target/debug/sylvander
```

### Internal compatible gateway

```sh
export ANTHROPIC_API_KEY=anything-here
export ANTHROPIC_BASE_URL=http://my-gateway.internal:9527
./target/debug/sylvander
```

### Local mock / dev server

```sh
export ANTHROPIC_API_KEY=dev
export ANTHROPIC_BASE_URL=http://127.0.0.1:9527
RUST_LOG=debug ./target/debug/sylvander
```

## Failures

```
ERROR: ANTHROPIC_API_KEY must be set. …
```

```
ERROR: ANTHROPIC_BASE_URL is set but empty — …
```

Both messages are emitted on stderr before `exit(1)` so a missing or
empty env var is impossible to miss.
