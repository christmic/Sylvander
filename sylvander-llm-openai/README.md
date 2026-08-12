# `sylvander-llm-openai`

Typed Rust support for the OpenAI Responses and Chat Completions protocols,
plus a provider-neutral `ModelProvider` adapter.

The crate deliberately follows the same boundary as
`sylvander-llm-anthropic`:

- `api/` owns direct wire types, HTTP, SSE framing, errors, and stream assembly.
- `convert/` is the only provider-neutral translation seam.
- `provider.rs` validates configured protocol features and adapts typed events.

Runtime supplies the base URL and credential explicitly. The crate never reads
provider configuration from process environment variables.

See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for the supported protocol
surface and verification contract.
