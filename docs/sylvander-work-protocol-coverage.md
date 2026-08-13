# Sylvander Work protocol coverage

> Status: active implementation ledger
>
> Verified against `sylvander-api/src/ui.rs` and Desktop commit `51abb1f8e`
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
| `Approve` | partial | render and submit Runtime `allowed_scopes`; currently `Once` only |
| `Answer` | complete | none |
| `Interrupt` | typed only | expose active-turn stop control |
| `ResolvePlan` | complete | none |
| `CancelTask` | complete | none |
| `DiscoverAgents` | complete | richer Agent selection metadata |
| `CreateSession` | complete, sparse defaults | workspace/config editor |
| `ListSessions`, `LoadSession` | complete | recovery metadata listed below |
| `RenameSession`, `ArchiveSession`, `DeleteSession` | complete | restore archived Session |
| `GetRuntimeInfo` | complete | model/permission selection commands |
| `GetSessionConfig`, `UpdateSessionConfig` | missing | Session settings surface |
| `SubmitFeedback` | missing | terminal feedback surface |
| `MemoryConfirmation` | missing | governed memory decision surface |
| `AgentAdmin`, `RegistryAdmin`, `UserProfile`, `IdentityBinding` | missing | administration/settings surfaces |
| `ReattachSession` | complete | selected Session reattaches only after a successful reconnect |
| `RestoreSession`, `ForkSession` | missing | archived/fork workflows |
| `GetContext`, `Compact` | missing | context and compaction surface |
| `PreviewWorkspaceRollback`, `RollbackWorkspace` | missing | reviewed rollback flow |
| `InspectCodingSession`, `AcceptCodingSession`, `DiscardCodingSession` | missing | changes/review surface |
| `SelectModel`, `SelectPermissions` | missing | Runtime-validated selectors |
| `Ping` | missing | explicit liveness journey |

## Server events

| Contract | Status | Remaining product work |
|---|---|---|
| `Welcome` | complete, native-owned | none |
| `ProtocolError` | complete, native-owned | public code/message/version range; transport details remain generic |
| `SessionCreated`, `SessionsList`, `SessionUpdated`, `SessionDeleted` | complete | restore/fork events use existing shapes |
| `SessionHistory` | partial | recovery notice/truncation complete; usage and source remain |
| `TextDelta`, `ThinkingDelta`, `ToolOutputDelta` | complete | visual progressive disclosure |
| `ToolCall`, `ToolResult`, `ToolRejected` | complete | bounded expandable tool input/details |
| `Done`, `Error`, `TurnInterrupted` | complete | feedback target |
| `ApprovalRequest` | partial | batch identity is retained; allowed scopes are not yet rendered |
| `AskUser` | complete | none |
| `PlanProposed`, `PlanUpdated` | complete | none |
| task lifecycle | complete | none |
| `RuntimeInfo` | partial | catalog/platform details and selectors |
| `ModelRetry`, `InteractionTimeout` | complete | typed cause/kind/recovery projection and matching decision dismissal |
| `IterationStart`, `IterationEnd` | missing | usage/cost projection |
| `SessionConfig` | missing | settings revision/provenance |
| feedback, memory, admin, profile, identity responses | missing | matching command surfaces |
| `ContextReport`, compaction lifecycle | missing | context inspector |
| workspace rollback lifecycle | missing | reviewed rollback result |
| coding Session lifecycle | missing | changes inspector |
| `OperationError`, `BoundaryDenied` | complete | safe operation-specific notice and bounded retry timing |
| `Pong` | missing | liveness state |

## Ordered implementation gates

1. Integrate the main Runtime lifecycle chain, including `TurnStarted`, in
   dependency order; do not cherry-pick only its enum.
2. Complete Session history/replay metadata and approval scopes.
3. Complete context, settings, model/permission selection, coding review, and
   rollback workflows.
4. Complete feedback, memory, identity, administration, attachments, and
   liveness surfaces with protocol and accessibility tests.

Every row moves to complete only with a typed command/event test and a product
behavior test. A visible control without a public command, or a union member
without reducer behavior, is not complete.
