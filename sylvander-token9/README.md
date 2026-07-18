# sylvander-token9

LLM gateway service for Sylvander, vendored via `git subtree` from
[christmic/token9](https://github.com/christmic/token9). The current
sylvander server (`sylvander-server`) forwards `POST /v1/messages`
traffic to a local gateway listening on a unix socket or HTTP port
(see `ANTHROPIC_BASE_URL` in `docs/server-env.md`); the upstream
endpoint it talks to is implemented by this crate family.

## Layout

```
sylvander-token9/
├─ token9-contracts/   shared wire types (request/response models)
├─ token9-server/      the gateway service binary
└─ token9-apps/        client surfaces (currently macOS menu-bar)
```

Module boundaries:

- [`token9-contracts/docs/ARCHITECTURE.md`](token9-contracts/docs/ARCHITECTURE.md)
  owns the serialized management/read DTO contract.
- [`token9-server/docs/ARCHITECTURE.md`](token9-server/docs/ARCHITECTURE.md)
  owns proxy routing, persistence, metering, local administration, and its
  deployment trust boundary.

## Workspace: intentionally nested

`sylvander-token9/Cargo.toml` declares its own `[workspace]` rather than
participating in Sylvander's root workspace. Reasons:

1. token9 pulls heavyweight deps that Sylvander doesn't need
   (sqlx, axum, tower-http, reqwest, rustls stack). Keeping them in a
   sibling workspace means `cargo build -p sylvander-tui` stays fast.
2. token9 ships independently upstream with its own release cadence;
   flattening would force version pin negotiations on every sync.
3. No Sylvander crate currently depends on `token9-contracts` or
   `token9-server` — the integration is purely runtime, over a Unix
   socket / HTTP. If that ever changes, revisit the workspace decision.

## Build

```bash
# Build only this subtree
cargo check --manifest-path sylvander-token9/Cargo.toml

# Build Sylvander's main workspace (won't touch this subtree)
cargo build --workspace
```

## Sync from upstream

```bash
# Bring latest from upstream master into this folder
git subtree pull --prefix=sylvander-token9 \
    git@github.com:christmic/token9.git master --squash

# Push local edits back upstream
git subtree push --prefix=sylvander-token9 \
    git@github.com:christmic/token9.git <branch-name>
```

## History

This folder was first added via `git subtree add --prefix=sylvander-token9
git@github.com:christmic/token9.git master --squash`. The squash keeps
Sylvander's history linear; the original commit chain lives upstream.
