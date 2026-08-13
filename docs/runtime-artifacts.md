# Runtime artifact architecture

This document defines how Sylvander retains outputs that are too large for an
Agent model context. It is normative for Agent and Runtime; MCP result storage
is a separate protocol concern and is not specified here.

## What an artifact is

An artifact is immutable content produced during one Agent turn that must
survive outside the model-visible conversation. Examples include a large plain
text tool result, a generated image, or a diagnostic bundle. An artifact is not:

- a workspace file that a tool may mutate;
- a workspace journal record used to roll back mutations;
- a Session transcript or message;
- an Evidence ledger entry, although Runtime may use the governed Evidence
  backend as the first artifact implementation;
- an MCP protocol result, whose transport and protocol policy are specified
  independently.

This distinction matters because workspace paths are execution authority while
artifact locators are references. Treating one as the other leaks host layout
into prompts and makes retention dependent on the current executor.

## Ownership and dependency direction

Agent owns only the provider-neutral policy and port:

1. decide whether one plain-text result exceeds its inline budget;
2. submit the call correlation, media type, and bytes through an asynchronous
   turn-bound artifact port;
3. replace the full result with a bounded preview and opaque locator;
4. report either the locator or a stable persistence failure.

Runtime owns every authority-bearing choice:

- user, Agent, Session, and turn binding;
- backend selection and encryption;
- locator generation and resolution;
- quotas, retention, deletion, and audit;
- storage health and recovery.

The port is attached through `AgentExecutionPorts`. It is already bound to the
current turn, so an Agent request cannot choose another tenant, Session,
directory, bucket, or encryption key. Agent continues to depend on
`sylvander-llm-core` only as a first-party crate.

```text
Runtime turn composition
    -> bound artifact port (identity + policy hidden)
        -> Agent compression policy (bytes + media type only)
            -> opaque locator
                -> model preview / Runtime event / client projection
```

## Model-visible contract

The model receives a preview and an opaque locator such as
`artifact:<identifier>`. It never receives an absolute path, object-store URL,
database key, tenant identifier, or encryption metadata. The locator is not
execution authority and cannot be opened by ordinary filesystem tools.

Artifact retrieval will be a separately authorized Runtime capability. Until
that capability exists, the locator supports transcript integrity, UI download,
audit, and operator recovery; it does not imply that the model can fetch the
discarded tail. The preview must therefore state explicitly that the output was
retained and truncated.

If no artifact port is installed, the budget layer is a no-op. If persistence
fails, the original result remains inline and the layer emits a bounded failure
report. Context reduction must never destroy the only available copy.

## Data and lifecycle rules

- Artifact writes are asynchronous and immutable.
- The accepted media type is explicit; the initial tool-result integration
  writes UTF-8 `text/plain`.
- Call identifiers are correlation only and never become filenames.
- The returned locator is opaque even when the backing store is local.
- Runtime rejects empty payloads and enforces a bounded maximum before writing.
- Successful persistence happens before conversation replacement.
- Repeated compression is idempotent because the replacement is below the
  configured threshold and is not persisted again.
- Rich `ToolResultContent::Blocks` remains inline until a media-aware policy is
  defined. Blind serialization would lose provider-neutral semantics.
- Artifact content is excluded from logs and health responses.

The first implementation uses Runtime's governed encrypted record storage. Its
record metadata binds the artifact to the current user, Agent, Session, turn,
and tool call; its public reference remains `artifact:<id>`. Production must not
enable this adapter without the governed encryption key.

## Health and observability

Artifact storage is a Runtime storage component, not an Agent health category.
Runtime reports only `Ready`, `Unverified`, or `Degraded`; it never returns
paths, payloads, keys, or raw backend failures. Agent compression events report
counts, estimated freed tokens, opaque locators, and stable failure classes.

Metrics may contain byte counts, latency, backend class, and success/failure.
They must not contain artifact content, locator values, call arguments, or
tenant identifiers. Runtime observability remains closed first-party code for
now, matching the platform architecture.

## Local upstream evidence

These repositories are design evidence, not wire specifications:

- Codex, commit `16fbfe557446a1af94da81e1144029ccc1311ad0`:
  `codex-rs/core/src/context_manager/history.rs` and
  `history_tests.rs` truncate function and custom-tool outputs using an
  explicit policy and preserve a visible truncation marker. The history does
  not embed a host path. Sylvander retains the explicit policy and marker, then
  adds a Runtime-owned durable reference.
- Claude Code source, commit
  `3da94d5e5f2b99c9d82b0d8f09448b04775cd41f`:
  `src/Tool.ts`, `src/constants/toolLimits.ts`, and
  `src/services/tools/toolExecution.ts` define per-tool thresholds, a global
  cap, a per-message aggregate budget, and processing before the result enters
  history. `FileReadTool` disables generic persistence to avoid a read loop.
  Its filesystem-path prompt contract is intentionally not adopted.
- pi coding agent, commit `11b5403fade1502a9a58a9cd4e9f983a3d1d734e`:
  `packages/coding-agent/src/core/tools/truncate.ts` returns explicit line/byte
  truncation metadata, while `core/messages.ts` may expose a full-output path.
  Sylvander adopts structured outcomes and rejects the path coupling.

## Implementation status

Implemented:

1. `ToolResultDisk` and the plaintext filesystem adapter were removed.
2. `TurnArtifactStore` is asynchronous and attached only through immutable
   `AgentExecutionPorts`.
3. The default compression pipeline includes L0 and safely becomes a no-op
   without a bound store.
4. Runtime binds user, Agent, Session, turn, and admission time before Agent
   execution, then stores restricted content through AES-256-GCM governance.
5. Runtime storage health reports Artifacts separately from Evidence; disabled
   governance is `Unverified`, a failed live governed-store probe is
   `Degraded`.
6. Tests cover encryption at rest, plaintext rejection, invalid payloads,
   opaque locators, UTF-8 preview boundaries, persistence failure, and the
   Anthropic, OpenAI, and DashScope model families.

Separately authorized artifact retrieval, client download projections, and
cross-repository backup/transaction policy remain future work.

MCP artifact handling is reviewed afterward and may delegate to this Runtime
service, but it must not shape this neutral Agent contract.
