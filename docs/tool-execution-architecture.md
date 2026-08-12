# Tool definition, execution, and sandbox architecture

This document records the implemented contract and the upstream evidence used
to choose it. It is intentionally limited to ordinary tools and their execution
environment. MCP transport, lifecycle, and remote authorization remain a
separate subsystem.

## Pinned upstream evidence

The following sources were downloaded and inspected locally. Paths name the
files in each pinned checkout so later reviews can reproduce the comparison.

| Upstream | Pinned revision | Evidence used |
|---|---|---|
| [OpenAI Codex](https://github.com/openai/codex) | `16fbfe557446a1af94da81e1144029ccc1311ad0` | `codex-rs/tools/src/tool_executor.rs` separates exposure and execution; `codex-rs/tools/src/tool_spec.rs` owns provider-neutral specifications. |
| [Google Gemini CLI](https://github.com/google-gemini/gemini-cli) | `4238b0b2b5e93aecff0193b4edf0cf57629e8138` | `packages/sdk/src/tool.ts` separates a definition from a prepared invocation; `packages/cli/src/utils/sandbox.ts` implements explicit macOS and container sandbox launch paths. |
| [Anthropic Sandbox Runtime](https://github.com/anthropic-experimental/sandbox-runtime) | `7f1792ab3db3ab9210e0a8fa74826dd59c63a5b4` | `README.md` and `src/sandbox/` require OS-enforced filesystem and network isolation and default network access to denied. |
| [Anthropic Claude Code public repository](https://github.com/anthropics/claude-code) | `681a8be245e7759a405e276b16ae69ea6b75076f` | `examples/settings/README.md` scopes the published sandbox contract to Bash; `CHANGELOG.md` documents fail-if-unavailable and sandbox bypass hardening. The proprietary tool implementation is not treated as source evidence. |
| `@mariozechner/pi-coding-agent` package | `0.73.1` | `dist/core/extensions/types.d.ts` keeps tool definition metadata beside execution; `dist/core/tools/file-mutation-queue.d.ts` exposes serialized file mutation. |

## Implemented contract

`ToolDefinition` publishes a stable `ToolSpec` containing a provider-neutral
JSON Schema, exposure, search hint, and authorization class. Its synchronous
`prepare` boundary validates untrusted model input and returns a
`ToolPreparation`. `ToolExecutor` never receives the raw model payload.

Registration binds the definition and executor into one trusted
`RegisteredTool`. Before authorization, the Agent resolves that registration
and freezes its implementation, normalized input, coordination mode, and
`ToolExecutionPolicy` in a `PreparedToolCall`. Authorization, audit, batch
coordination, and execution therefore refer to the same immutable call.

Side-effecting calls are exclusive within one model-produced batch. Read-only
tools may run in parallel. A tool may make this decision from validated input;
for example, read-only Git operations prepare a parallel read-only process
policy instead of inheriting the generic terminal default.

Deferred exposure and `tool_search` are generic registry capabilities. They do
not silently change MCP exposure: an MCP adapter must choose its own policy and
continues to preserve the server-provided schema.

## Execution environment

Structured file operations do not launch untrusted code. They execute through
`WorkspaceExecutor`, whose typed target and relative-path validation provide
the workspace boundary. They do not claim an OS process sandbox.

Any prepared call that launches a process requires all of these properties:

- filesystem authority restricted to the declared workspace access;
- network denied by the enforcing boundary;
- resource limits enforced for the process tree.

`WorkspaceExecutor::process_isolation()` defaults to unavailable. Local and SSH
executors retain that default, so Command and Git calls fail closed instead of
running unsandboxed. The OCI container executor is currently the only enforcing
backend: every operation uses a disposable container with an explicit bind
mode, `--network=none`, read-only root filesystem, private temporary storage,
no new privileges, dropped capabilities, and memory, CPU, and PID ceilings.

Agent owns only this port and its neutral routing policy. Runtime owns all
three concrete executors. Runtime boot constructs one immutable
`RuntimeExecutionService`, resolves transport credentials once, validates
exact target identities, and injects the same snapshot into initial and lazy
Agent revisions. `local` is a named built-in target; it is never substituted
for an unknown target. Constructing a `ToolContext` or attaching a filesystem
root never grants host-local access.

The execution service owns adapter selection and target resolution. The
sandbox adapter itself owns the enforcing operation boundary: disposable
process creation, mounts, network namespace, resource limits, filtered
environment, cancellation, bounded streams, artifact collection, violation
reporting, and cleanup. Agent uses that service; Agent and Runtime control
planes do not run inside each per-tool sandbox.

This release does not claim native Seatbelt, bubblewrap, Windows token/WFP, or
approved-network proxy support. `ToolNetworkPolicy::FullAfterApproval` remains
non-executable until a backend can enforce that policy without falling back to
unrestricted host networking.

## Final design decision

The durable boundary is **definition -> preparation -> authorization ->
environment validation -> execution -> terminal audit**. Provider protocols
only translate the neutral specification at the wire edge. Models do not
select executors, workspaces, sandbox strength, network authority, or owners.

This combines Codex's specification/executor separation, Gemini's prepared
invocation boundary, Anthropic's fail-closed OS isolation requirements, Claude
Code's public Bash sandbox scope, and pi's conservative mutation coordination.
It deliberately avoids pretending that a configuration label is a sandbox.
