# Persistent tool approval

`sylvander-agent` owns approval decisions. Transports can present a request and
return the user's selected lifetime, but they cannot create, widen, or reuse a
grant. Runtime identity and the immutable turn snapshot remain authoritative.

## Grant identity

A persistent grant matches only one exact tuple:

```text
stable UserId
+ AgentId
+ approval policy revision
+ capability revision
+ operation
+ resource fingerprint
```

- `UserId` comes from a Runtime-authenticated session lease. Unauthenticated
  bus sessions are never offered persistent scope.
- The policy revision is a domain-separated SHA-256 revision of the effective
  permission profile and ordered static approval rules.
- The capability revision is a domain-separated SHA-256 revision of the frozen
  turn tool catalog, including tool names, descriptions, schemas, and
  before-tool hooks.
- The operation is the exact tool name.
- The resource fingerprint is a domain-separated SHA-256 digest of canonical
  JSON input. Raw paths, commands, content, and other arguments are not written
  to the approval store.

Changing any dimension is an automatic invalidation: the new request no longer
matches the old key and must be approved again. This includes relinking to
another stable user, activating another Agent, changing permissions or rules,
refreshing MCP/dynamic tools, changing hooks, selecting another operation, or
changing any argument.

## Turn and lifetime behavior

Dynamic tool sources are frozen once at the start of a turn. The same immutable
tool snapshot is used for the model request, approval revision, and execution.
A capability refresh applies to the next turn.

- `once` approves only the pending call and writes no grant.
- `session` stores the exact grant under that session and removes it when the
  session leaves the Agent run.
- `persistent` writes the exact grant to the configured durable store and can
  match another session only when all six dimensions remain identical.

## Store and failure behavior

The current store schema is version 1:

```json
{
  "schema_version": 1,
  "grants": [
    {
      "user_id": "user-123",
      "agent_id": "sylvander",
      "policy_revision": "sha256:...",
      "capability_revision": "sha256:...",
      "operation": "write",
      "resource_fingerprint": "sha256:..."
    }
  ]
}
```

The writer sorts grants, creates a mode `0600` temporary file on Unix, syncs
file contents, atomically renames it, and syncs the containing directory.
Agent runs in the server process share one path-keyed state and writer lock, so
revision workers cannot overwrite one another's grants. Only one server process
may own a store path. Write failure rolls back the in-memory insertion. Startup
rejects oversized, malformed, duplicate, unknown-version, and legacy
fingerprint-only files. The pre-release latest-only policy intentionally
provides no legacy fallback.

Operational recovery is explicit:

1. Stop the server.
2. Preserve the rejected file for audit.
3. Remove or replace it with a validated current-schema file.
4. Restart; an absent file starts with no persistent grants.

Never hand-edit a live store. Rotating the file to an empty current store
revokes every persistent grant immediately after restart.
