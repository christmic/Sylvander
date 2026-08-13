# Sylvander Work protocol coverage

> Status: active implementation ledger
>
> Verified against `sylvander-api/src/ui.rs` and the current Desktop source on
> 2026-08-13.

## Purpose

This is the Desktop service-edge SSOT. “Typed” means the native gateway can
serialize or deserialize the public Rust contract. “Projected” means React
turns the message into user-visible state. Neither status implies Runtime
execution, persistence, or policy ownership moved into Desktop.

## Client commands

| Contract | Status | Remaining product work |
|---|---|---|
| `Hello` | complete, native-owned | none |
| `Chat` | text and authoritative turn admission complete | attachments |
| `Approve` | complete | renders only Runtime `allowed_scopes`, defaulting to protocol `Once` |
| `Answer` | complete | none |
| `Interrupt` | complete | stop remains pending until a Runtime terminal event |
| `ResolvePlan` | complete | none |
| `CancelTask` | complete | none |
| `DiscoverAgents` | complete | richer Agent selection metadata |
| `CreateSession` | complete, sparse defaults | workspace/config editor |
| `ListSessions`, `LoadSession` | complete end-to-end | archive-aware query is split by Runtime `archived` truth |
| `RenameSession`, `ArchiveSession`, `DeleteSession` | complete end-to-end | none |
| `GetRuntimeInfo` | complete end-to-end | Agent-scoped Runtime snapshot; no transport-local assembly |
| `GetSessionConfig`, `UpdateSessionConfig` | complete | revision-bound field patch preserves omitted write-only state |
| `SubmitFeedback` | complete | private rating and optional note preserve Runtime's opaque target |
| `MemoryConfirmation` | designed | implement capability-gated latest-only list and explicit revision-bound decision |
| `AgentAdmin`, `RegistryAdmin`, `UserProfile`, `IdentityBinding` | missing | administration/settings surfaces |
| `ReattachSession` | complete end-to-end | 4 MiB bounded live-event replay; truncation is failed-visible |
| `ForkSession` | complete end-to-end for checkpoints | completed-turn rewind editor |
| `RestoreSession` | complete end-to-end | UI waits for `SessionUpdated` before refreshing active/archive lists |
| `GetContext`, `Compact` | complete end-to-end | none |
| `PreviewWorkspaceRollback`, `RollbackWorkspace` | complete end-to-end | none |
| `InspectCodingSession`, `AcceptCodingSession`, `DiscardCodingSession` | complete end-to-end | none |
| `SelectModel`, `SelectPermissions` | complete | provider-qualified catalog and typed permission profile |
| `Ping` | complete | explicit user-requested liveness round trip |

## Server events

| Contract | Status | Remaining product work |
|---|---|---|
| `Welcome` | complete, native-owned | none |
| `ProtocolError` | complete, native-owned | public code/message/version range; transport details remain generic |
| `SessionCreated`, `SessionsList`, `SessionUpdated`, `SessionDeleted` | complete | restore/fork events use existing shapes |
| `SessionHistory` | complete | messages, usage, cost, source, recovery notice, and truncation |
| `TurnStarted` | complete | Runtime turn identity is the sole active-state authority |
| `TextDelta`, `ThinkingDelta`, `ToolOutputDelta` | complete | visual progressive disclosure |
| `ToolCall`, `ToolResult`, `ToolRejected` | complete | bounded expandable tool input/details |
| `Done`, `Error`, `TurnInterrupted` | complete | feedback target |
| `ApprovalRequest` | complete | batch identity and allowed authorization scopes retained |
| `AskUser` | complete | none |
| `PlanProposed`, `PlanUpdated` | complete | none |
| task lifecycle | complete | none |
| `RuntimeInfo` | complete | qualified model, catalog, permissions, capabilities, request limit, and platform remain Runtime-owned |
| `ModelRetry`, `InteractionTimeout` | complete | typed cause/kind/recovery projection and matching decision dismissal |
| `IterationStart`, `IterationEnd` | complete | cumulative usage/cost projection; never implies turn admission |
| `SessionConfig` | complete | sparse overrides, effective values, revision, and provenance |
| feedback responses | complete | acknowledgement settles only an in-flight feedback submission |
| memory responses | designed | pending replaces selected-Session queue; recorded settles matching candidate; error preserves it |
| admin, profile, identity responses | missing | matching command surfaces |
| `ContextReport`, compaction lifecycle | complete | provider usage, sources, cache, completion/failure |
| workspace rollback lifecycle | complete | preview, restored files, and failure |
| coding Session lifecycle | complete | diff, accepted, discarded, and operation failure |
| `OperationError`, `BoundaryDenied` | complete | safe operation-specific notice and bounded retry timing |
| `Pong` | complete | checking becomes healthy only on Runtime response |

## Ordered implementation gates

1. Complete feedback, memory, identity, administration, attachments, and
   liveness surfaces with protocol and accessibility tests.

Every row moves to complete only with a typed command/event test and a product
behavior test. A visible control without a public command, or a union member
without reducer behavior, is not complete.
