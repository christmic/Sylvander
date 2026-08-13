# MCP runtime

Sylvander treats each configured MCP server as a supervised external tool
source. The production local transport is MCP 2025-11-25 over stdio; remote
workspace execution does not change this protocol boundary.

## Official protocol evidence

The official [`modelcontextprotocol/rust-sdk`](https://github.com/modelcontextprotocol/rust-sdk)
was downloaded locally and pinned at
`a50a73fda2cd55f87633a280b430f539b1094234`. The implementation was compared
with `crates/rmcp/src/model/tool.rs` for the required JSON Schema object and
untrusted annotation semantics, `crates/rmcp/src/service/client.rs` for complete
cursor traversal, `crates/rmcp/src/transport/child_process.rs` for child
lifecycle, and the corresponding SDK tests. Wire requirements come from the
official MCP 2025-11-25 lifecycle, transport, and tools specifications rather
than compatible third-party servers.

Tool discovery follows every `nextCursor` page before atomically publishing a
new catalog. It rejects a missing or non-object `inputSchema`, repeated cursors,
more than 32 pages, and more than 4096 tools. Server annotations remain
untrusted hints and do not grant authorization, concurrency, filesystem, or
network authority.

## Lifecycle

Runtime composition:

1. resolves environment secret references without storing their values in the
   Agent definition;
2. starts the configured command with piped stdin/stdout and kill-on-drop;
3. negotiates the exact protocol revision and sends the initialized
   notification;
4. discovers tools and, when advertised, resources;
5. atomically publishes collision-safe `mcp__server__tool` names;
6. probes health every 30 seconds and reconnects after a recoverable transport
   failure;
7. retains process ownership inside the configured Runtime revision; dropping
   that revision terminates the child through Tokio's kill-on-drop boundary.

The stdio child starts with an empty environment and receives only values
explicitly resolved by Runtime composition. This blocks ambient HOME, proxy,
provider, and cloud credentials from crossing the process boundary. Environment
scrubbing is not an OS sandbox: a future persistent-process environment port
must additionally enforce filesystem, network, resource, cancellation, and
cleanup policy for the complete MCP server lifetime.

Explicit awaited MCP shutdown during Runtime drain is not yet composed. This
is a lifecycle gap: kill-on-drop prevents an orphaned process, but does not
provide the same observable graceful-drain guarantee as an awaited stop.

An uncertain in-flight call is never replayed. After reconnection the complete
tool and resource catalog is refreshed for the next model iteration.

## Bounds and cancellation

Every request has a configured client deadline. A timeout sends
`notifications/cancelled` before returning a typed timeout. Dropping the
request future, including a user-interrupted Agent turn, also emits protocol
cancellation asynchronously. The child remains kill-on-drop as the final
process boundary.

Frames are limited to 16 MiB. Model- and UI-facing results are Unicode-safe,
bounded head/tail summaries. With Runtime evidence encryption configured,
complete JSON results are routed to the tenant/user-scoped governed artifact
store and the summary carries only an opaque `evidence-artifact:` locator.
Agent code never writes plaintext result files below the Runtime data
directory. Inline binary data is represented without copying its encoded
payload into the transcript.

## Inspection

The ordinary platform snapshot reports active, degraded, or unavailable
health plus tool/resource counts, process generation, reconnect count,
cancellation count, authentication state, capabilities, and reloadability.
It never exposes environment values, arguments, raw results, or full command
paths.

MCP prompts, subscriptions, and non-stdio transports are optional protocol
extensions rather than implicit fallbacks. Unsupported capabilities are not
advertised to the model.
