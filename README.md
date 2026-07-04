# Sylvander v2

Sylvander v2 is an AI Agent framework written in Rust. This is a fresh start
(`v2`); the original `v1` code is preserved at `../Sylvander_archive` and is
not referenced in v2 development.

## Workspace Layout

```
.
├── Cargo.toml                     workspace root
├── sylvander-llm-anthropic/       M1: Anthropic protocol SDK
└── (future crates)                M2/M3: sylvander-llm-openai, sylvander-agent, ...
```

## Milestones

| Milestone | Status | Crate |
|-----------|--------|-------|
| **M1 Protocol SDK** | in progress | `sylvander-llm-anthropic` |
| **M2 Agent Loop** | pending | `sylvander-agent` |
| **M3 Tool System** | pending | `sylvander-agent` |

See `../projects/Sylvander/designs/m1-m2-m3-roadmap.md` (in Oraculo) for the
product-first roadmap: **M2 Agent Loop comes before M3 Tool System**, because
the loop drives tool design rather than the reverse.

## Build

```bash
cargo build --workspace
cargo test --workspace
cargo doc --workspace --no-deps
```

## Conventions

- **MSRV**: 1.96, edition 2024
- **Async**: tokio (multi-thread runtime)
- **HTTP**: reqwest (rustls only, no OpenSSL)
- **Errors**: `thiserror` for typed crate errors
- **Lints**: workspace `unsafe_code = "deny"` + clippy pedantic (with allow-list)
- **Tests**: `#[cfg(test)]` unit + `tests/` integration (wiremock)

## License

MIT