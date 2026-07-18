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
| `SYLVANDER_MODEL`    | Model id the gateway recognizes (e.g. `MiniMax-M3`). Non-empty required because the wrong id silently 404s on the gateway. |

## Optional

| Variable             | Default                       | Notes                                          |
|----------------------|-------------------------------|------------------------------------------------|
| `SYLVANDER_SOCKET`   | `/tmp/sylvander.sock`         | Path the TUI client uses for its Unix socket. |
| `HTTP_ADDR`          | `127.0.0.1:8080`              | Address the debug HTTP channel binds (always on). |
| `DINGTALK_APP_KEY`   | —                             | DingTalk channel only enabled when *both* this and `DINGTALK_APP_SECRET` are set. |
| `DINGTALK_APP_SECRET`| —                             | (ditto)                                         |
| `SYLVANDER_APPROVAL` | unset                         | Set to any value to enable tool approval gate.  |
| `SYLVANDER_APPROVAL_STORE` | unset | Current-schema JSON store for durable six-dimensional approval grants. Persistent scope additionally requires a Runtime-authenticated stable identity and `SYLVANDER_APPROVAL`; malformed, legacy, or unknown schemas fail startup. |
| `SYLVANDER_MODELS` | primary model only | Comma-separated model ids exposed to `/model`; `SYLVANDER_MODEL` is inserted if omitted. |
| `SYLVANDER_REASONING_MODELS` | empty | Comma-separated subset of model ids that support low/medium/high reasoning. Other models advertise `off` only. |
| `SYLVANDER_DEPRECATED_MODELS` | empty | Comma-separated `model` or `model=replacement` entries. The lifecycle is advertised to clients; deprecated models remain selectable for old sessions. |
| `SYLVANDER_MODEL_PRICING` | empty | Comma-separated `model=input:output[:cache_write:cache_read]` prices in USD per million tokens. Invalid values fail startup; omitted prices remain explicitly unknown. |
| `SYLVANDER_SESSION_DB` | `$XDG_DATA_HOME/sylvander/sessions.db`, or `$HOME/.local/share/sylvander/sessions.db` | Persistent SQLite session/history database. |
| `SYLVANDER_WORKSPACE_JOURNAL` | sibling `workspace-journal/` beside the session database | Durable pre/post snapshots used only for confirmed rollback of Agent Write/Edit calls. |
| `RUST_LOG`           | `info`                        | Standard tracing-subscriber filter.            |
| `SYLVANDER_LOG_FORMAT` | text | Set to `json` for flattened JSON tracing suitable for log collectors. |

## Why is `SYLVANDER_MODEL` required?

The agent loop calls `POST {ANTHROPIC_BASE_URL}/v1/messages` with
`model={SYLVANDER_MODEL}`. If the gateway doesn't recognize the model
id it returns **404** — and unlike transient 5xx, the agent loop does
not back off on 4xx and will hammer the endpoint until the operator
intervenes. Making the model required at startup ensures a typo or
unknown id surfaces before any traffic leaves.

If you see `Anthropic API error (status 404): unknown: (no message)` in
the server log, the gateway does not have the configured model id —
check `SYLVANDER_MODEL` against the gateway's `/v1/models` list.

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
export SYLVANDER_MODEL=fast-code
export SYLVANDER_MODELS=fast-code,deep-code
export SYLVANDER_REASONING_MODELS=deep-code
export SYLVANDER_DEPRECATED_MODELS=fast-code=deep-code
export SYLVANDER_MODEL_PRICING=fast-code=0.10:0.40,deep-code=3:15:3.75:0.30
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
