# token9-contracts module boundary

`token9-contracts` is the serialized management/read contract shared by the
token9 server and generated client surfaces. It is a leaf crate: it owns data
transfer objects and their Serde/typeshare representation, but it does not own
routing, persistence, authentication, transport, or provider execution.

## Owned contract

The crate defines response DTOs for:

- aggregated request/token statistics;
- redacted Provider and logical Model inventory;
- observed Provider rate-limit snapshots;
- logical tool-identification rules; and
- raw observed tool identifiers and their logical mapping.

All DTOs are transport values. They must not acquire database handles, runtime
services, or behavior that makes deserialization perform work. Numeric
typeshare annotations are part of the generated-client ABI and must be changed
with the client generator and server response together.

`ProviderDto.api_key` is allowed to carry only the server-produced masked
representation. This type does not make a raw secret safe; the token9 server
remains responsible for redaction before constructing it.

## Non-owned contracts

The proxied Anthropic/OpenAI request and response bodies are deliberately not
normalized here. token9 forwards those vendor payloads through the
`token9-server` proxy path. Provider selection, fallback, metering, rate-limit
capture, admin mutations, and SQLite schemas are server-owned.

## Change and verification

This is a pre-release latest-interface contract. There is no compatibility
shim: update the Rust DTO, generated client types, server construction, and
tests in one change.

From `sylvander-token9/`:

```sh
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
```
