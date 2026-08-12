# `sylvander-llm-dashscope`

Typed Rust support for native DashScope Generation plus a provider-neutral
`ModelProvider` adapter.

The crate follows the same layered boundary as `sylvander-llm-anthropic`:

- `api/` owns native request/response types, HTTP, SSE, errors, and assembly.
- `convert.rs` is the only provider-neutral translation seam.
- `provider.rs` validates features and adapts typed stream events.

Endpoint and credential configuration are explicit constructor inputs. See
[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for protocol scope.
