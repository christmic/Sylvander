# Typed turn context

Every authenticated Worker turn uses one server-authored context pipeline. The
model and client cannot choose owners, inject provenance, raise budgets, or
change precedence.

## Canonical layers

The system prompt is composed in this order:

1. `Safety` — immutable Runtime and organization policy.
2. `Agent` — the selected Agent revision plus the qualified model prompt
   profile.
3. `UserProfile` — the latest compact interaction contract for the
   authenticated `UserId`.
4. `RelationshipMemory` — relevant active records for the exact
   `(UserId, AgentId)` relationship.
5. `WorkspaceKnowledge` — hierarchical workspace instructions followed by
   relevant, read-only workspace search results.
6. `Session` — the explicit current-session system override. Transcript
   history and current input remain typed model messages and are not duplicated
   into the system prompt.

Later layers are more specific, but cannot override Safety, organization
policy, Agent identity, authorization, or tool policy. Retrieved memory and
workspace text is data from an untrusted content boundary, never authority.

## Manifest and provenance

`TurnContextManifest` records:

- schema version and aggregate SHA-256;
- layer type and numeric precedence;
- the configured byte, token-estimate, and item budgets;
- actual byte and token estimates;
- included item source, stable reference, optional revision, SHA-256, and
  relevance score;
- omitted-item count.

Prompt bodies are redacted from `Debug`. Provider failures and retrieval errors
are mapped to stable content-safe errors.

## Retrieval contract

Dynamic context is never loaded with an unbounded or empty-query scan.

- The current user input is normalized into at most four meaningful terms.
- Each relationship-memory search has an explicit result limit. Results are
  deduplicated, ranked, and checked again for expiry and supersession.
- Workspace lookup uses `WorkspaceExecutor::search` on the exact effective
  task-workspace target. Each search has result, line, byte, and timeout
  limits. A timed-out term contributes no result; another execution error is
  content-safely rejected.
- Zero-relevance candidates are excluded. Remaining candidates are
  deterministically ordered and admitted only while all layer limits fit.
- Required static layers fail closed when oversized. They are never silently
  truncated. Retrieved candidates may be omitted and the manifest reports the
  count.

Production relationship reads use the Runtime-injected durable
`MemoryStore`. Workspace retrieval uses the same location-neutral executor as
tools, so local, SSH, container, and managed targets share the contract without
exposing their transport to the Agent.

## Configuration and extension

`TurnContextBudgets` provides production defaults and
`AgentRunBuilder::turn_context_budgets` allows Runtime configuration to replace
them as one immutable per-run policy. A running turn snapshots these budgets
alongside its model, permission, and capability configuration.

New context sources must:

1. map into one existing precedence layer or introduce a reviewed typed layer;
2. carry server-derived provenance and a stable revision where one exists;
3. support bounded relevance retrieval rather than full-store prompt
   injection;
4. define expiry/supersession visibility;
5. add tests under `sylvander-agent/tests/`, never inline test bodies in
   `src/`.
