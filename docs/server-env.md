# Server environment

The server accepts exactly one configuration entry point:
`SYLVANDER_CONFIG`. It must name the latest-version TOML document described in
[`server-configuration.md`](server-configuration.md). If the variable is
missing, empty, non-Unicode, unreadable, or points to an old/unknown schema,
startup fails before Runtime composition or listener activation.

```sh
export SYLVANDER_CONFIG=/etc/sylvander/server.toml
./target/release/sylvander
```

There is no environment-only server mode, legacy conversion, model-list
fallback, or implicit provider. Provider/model definitions, channels, Agents,
execution targets, modes, limits, prompt profiles, and revision-bearing
configuration live in the TOML document.

## Process environment read directly

| Variable | Required | Purpose |
|---|:---:|---|
| `SYLVANDER_CONFIG` | yes | Absolute or process-relative path to the current server configuration |
| `RUST_LOG` | no | Standard `tracing_subscriber::EnvFilter`; defaults to `info` |
| `SYLVANDER_LOG_FORMAT` | no | `json` selects flattened JSON tracing; any other value selects human-readable tracing |

## Secret-reference environment

The TOML schema may deliberately refer to a secret by environment-variable
name:

```toml
[model_providers.api_key]
source = "env"
name = "ANTHROPIC_API_KEY"
```

Such a variable is required only because the selected configuration references
it. The value is resolved through `SystemSecretResolver`, bounded, redacted,
and never promoted into a public configuration object. The same mechanism
applies to provider credentials, channel credentials, identity keys, SSH
identity paths, evidence encryption, and the memory integrity boundary.

Prefer file-backed secret references for service managers that mount secrets:

```toml
[model_providers.api_key]
source = "file"
path = "/run/secrets/anthropic-api-key"
```

## Model identity

Models are always selected by the qualified pair
`(provider_id, model_id)`. The Agent default, allowed overrides, prompt-profile
selectors, session effective configuration, and administration protocol all
use that identity. Bare `SYLVANDER_MODEL`, comma-separated model environment
lists, and same-name guessing are not supported.

Each durable session must carry the current effective-configuration revision,
immutable Agent/Provider/Model pins, and prompt manifest. Missing pins,
missing manifests, old schemas, or unknown schemas fail closed; the server does
not repair them through environment defaults.

## Common startup failures

```text
SYLVANDER_CONFIG must name the latest-version server configuration
```

Set the variable to the maintained schema-v1 document and restart. For a
configuration validation error, correct the TOML or referenced secret; do not
remove fields until an older shape happens to parse.
