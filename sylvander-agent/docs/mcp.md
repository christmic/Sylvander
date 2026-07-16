# MCP runtime

Sylvander treats each configured MCP server as a supervised external tool
source. The production local transport is MCP 2025-11-25 over stdio; remote
workspace execution does not change this protocol boundary.

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
7. shuts the owned process down during Runtime drain.

An uncertain in-flight call is never replayed. After reconnection the complete
tool and resource catalog is refreshed for the next model iteration.

## Bounds and cancellation

Every request has a configured client deadline. A timeout sends
`notifications/cancelled` before returning a typed timeout. Dropping the
request future, including a user-interrupted Agent turn, also emits protocol
cancellation asynchronously. The child remains kill-on-drop as the final
process boundary.

Frames are limited to 16 MiB. Model- and UI-facing results are Unicode-safe,
bounded head/tail summaries; complete JSON results can be persisted as
artifacts below the Runtime data directory. Inline binary data is represented
without copying its encoded payload into the transcript.

## Inspection

The ordinary platform snapshot reports active, degraded, or unavailable
health plus tool/resource counts, process generation, reconnect count,
cancellation count, authentication state, capabilities, and reloadability.
It never exposes environment values, arguments, raw results, or full command
paths.

MCP prompts, subscriptions, and non-stdio transports are optional protocol
extensions rather than implicit fallbacks. Unsupported capabilities are not
advertised to the model.
