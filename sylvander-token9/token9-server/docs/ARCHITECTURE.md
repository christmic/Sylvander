# token9-server module boundary

`token9-server` is Sylvander's separately built local LLM gateway. It accepts
vendor-compatible HTTP requests, resolves a logical model to one or more
configured Provider targets, forwards the request, records bounded usage and
routing facts, and exposes local administration/statistics endpoints. It is a
separate Cargo workspace and process; no Sylvander Runtime crate links it as a
library.

## Internal ownership

```text
CLI / Axum routes
  -> RouteTable + selection plan
  -> reqwest upstream transport
  -> dialect-aware response metering
  -> SQLite configuration and request ledger
```

- `config` owns bootstrap bind/domain/database settings. Provider, key, route,
  logical-model, and tool-rule state lives in SQLite.
- `admin` and `cli` mutate that same store; the HTTP reload endpoint refreshes
  the in-process route/tool snapshots after a change.
- `routetable` and `select` produce an ordered attempt plan from priority,
  weight, available keys, and observed rate limits.
- `proxy` owns vendor dialect adaptation, upstream attempts, streaming, and
  the terminal request record.
- `metering`, `ratelimit`, `stats`, and `tool` parse bounded operational facts.
- `store` owns the current SQLite schema and persistence API.
- `hosts` is an optional local convenience for the branded loopback name. A
  failed or declined hosts-file change does not prevent loopback operation.

The public management/read response types come from `token9-contracts`.
Vendor request bodies remain vendor-shaped and are not part of that crate.

## Trust and deployment boundary

The current server is a **local trusted proxy**, not an Internet-facing
multi-tenant gateway:

- the default bind address is `127.0.0.1`;
- admin routes do not provide a remote authentication layer;
- request-body size is delegated to the upstream Provider;
- Provider keys are stored in the token9 SQLite configuration store and are
  masked only when returned through management DTOs; and
- `/etc/hosts` mutation may request local administrator authorization.

A deployment must keep this process on a trusted loopback/private boundary or
add an authenticated reverse-proxy boundary before exposing it. These facts
must not be confused with Sylvander Runtime's renewable credential-lease and
credential-audit contracts.

## Lifecycle and failure contract

Startup opens/creates the SQLite database, seeds default settings and tool
rules, loads route snapshots, and then binds the configured address. Unknown
dialects and invalid configuration fail startup. A proxy request records the
selected logical/real model, attempts, routing reason, metering, latency, and a
bounded error fact; a failed Provider attempt may continue only according to
the ordered route plan.

The nested workspace is verified independently from the root Sylvander Cargo
workspace:

```sh
cd sylvander-token9
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
```
