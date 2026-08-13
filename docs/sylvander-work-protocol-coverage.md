# Sylvander Work protocol coverage

> Status: active implementation ledger
>
> Verified against `sylvander-api/src/ui.rs` and Desktop commit `8a4a9707c`
> on 2026-08-13.

## Purpose

This is the Desktop service-edge SSOT. “Typed” means the native gateway can
serialize or deserialize the public Rust contract. “Projected” means React
turns the message into user-visible state. Neither status implies Runtime
execution, persistence, or policy ownership moved into Desktop.

## Client commands

| Contract | Status | Remaining product work |
|---|---|---|
| `Hello` | complete, native-owned | none |
| `Chat` | text complete | attachments and authoritative `TurnStarted` integration |
| `Approve` | complete | renders only Runtime `allowed_scopes`, defaulting to protocol `Once` |
| `Answer` | complete | none |
| `Interrupt` | complete | stop remains pending until a Runtime terminal event |
| `ResolvePlan` | complete | none |
| `CancelTask` | complete | none |
| `DiscoverAgents` | complete | richer Agent selection metadata |
| `CreateSession` | complete, sparse defaults | workspace/config editor |
| `ListSessions`, `LoadSession` | complete end-to-end | live-turn recovery is separate |
| `RenameSession`, `ArchiveSession`, `DeleteSession` | complete end-to-end | none |
| `GetRuntimeInfo` | UI complete, WebSocket missing | add transport dispatch and runtime snapshot |
| `GetSessionConfig`, `UpdateSessionConfig` | complete | revision-bound field patch preserves omitted write-only state |
| `SubmitFeedback` | missing | terminal feedback surface |
| `MemoryConfirmation` | missing | governed memory decision surface |
| `AgentAdmin`, `RegistryAdmin`, `UserProfile`, `IdentityBinding` | missing | administration/settings surfaces |
| `ReattachSession` | complete end-to-end | 4 MiB bounded live-event replay; truncation is failed-visible |
| `RestoreSession`, `ForkSession` | WebSocket complete, UI missing | archived/fork workflows |
| `GetContext`, `Compact` | UI complete, WebSocket missing | transport dispatch to Runtime-owned lifecycle |
| `PreviewWorkspaceRollback`, `RollbackWorkspace` | UI complete, WebSocket missing | transport dispatch for two-phase rollback |
| `InspectCodingSession`, `AcceptCodingSession`, `DiscardCodingSession` | UI complete, WebSocket missing | transport dispatch for review and decision |
| `SelectModel`, `SelectPermissions` | complete | provider-qualified catalog and typed permission profile |
| `Ping` | complete | explicit user-requested liveness round trip |

## Server events

| Contract | Status | Remaining product work |
|---|---|---|
| `Welcome` | complete, native-owned | none |
| `ProtocolError` | complete, native-owned | public code/message/version range; transport details remain generic |
| `SessionCreated`, `SessionsList`, `SessionUpdated`, `SessionDeleted` | complete | restore/fork events use existing shapes |
| `SessionHistory` | complete | messages, usage, cost, source, recovery notice, and truncation |
| `TextDelta`, `ThinkingDelta`, `ToolOutputDelta` | complete | visual progressive disclosure |
| `ToolCall`, `ToolResult`, `ToolRejected` | complete | bounded expandable tool input/details |
| `Done`, `Error`, `TurnInterrupted` | complete | feedback target |
| `ApprovalRequest` | complete | batch identity and allowed authorization scopes retained |
| `AskUser` | complete | none |
| `PlanProposed`, `PlanUpdated` | complete | none |
| task lifecycle | complete | none |
| `RuntimeInfo` | partial | catalog/platform details and selectors |
| `ModelRetry`, `InteractionTimeout` | complete | typed cause/kind/recovery projection and matching decision dismissal |
| `IterationStart`, `IterationEnd` | complete | active state and cumulative usage/cost projection |
| `SessionConfig` | complete | sparse overrides, effective values, revision, and provenance |
| feedback, memory, admin, profile, identity responses | missing | matching command surfaces |
| `ContextReport`, compaction lifecycle | complete | provider usage, sources, cache, completion/failure |
| workspace rollback lifecycle | complete | preview, restored files, and failure |
| coding Session lifecycle | complete | diff, accepted, discarded, and operation failure |
| `OperationError`, `BoundaryDenied` | complete | safe operation-specific notice and bounded retry timing |
| `Pong` | complete | checking becomes healthy only on Runtime response |

## Ordered implementation gates

1. Complete WebSocket parity for Runtime info, context, compaction, coding
   review, and rollback before marking their existing UI projections complete.
2. Integrate the authoritative Runtime lifecycle chain, including
   `TurnStarted`; do not infer it from local submission.
3. Complete feedback, memory, identity, administration, attachments, and
   liveness surfaces with protocol and accessibility tests.

Every row moves to complete only with a typed command/event test and a product
behavior test. A visible control without a public command, or a union member
without reducer behavior, is not complete.
