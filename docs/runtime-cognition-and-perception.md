# Runtime cognition and perception

## Identity boundary

An `AgentInstance` is a first-class Session participant. It owns responsibility,
workflow state, topology relations, mailbox identity, capability bounds, and a
workspace view. A model is not an Agent.

Primary, fast-draft, deliberation, critic, vision, audio, and document models
are internal cognitive roles of one Agent. They receive no Agent mailbox,
tools, memory authority, topology identity, or workspace lease. The primary
model remains responsible for the model-visible answer.

## Built-in perception boundary

Typed image, audio, and document blocks cross the provider-neutral LLM
boundary directly when the primary model supports the required capability. If
it does not, Runtime may identify one configured same-Agent specialist.

Specialists are evaluation-only by default. `evaluate_perception_specialist`
requires an authenticated Session capability, a running durable turn, an exact
configured cognitive role, an allowlisted provider-qualified model, encrypted
artifact storage, and a stable invocation ID. Ordinary turn routing does not
call this method automatically.

The specialist receives one media block, a bounded modality instruction, no
tools, no conversation history, and at most 4096 output tokens. Its response
may contain text and reasoning only. Tool calls, paused responses, empty text,
wrong-model terminals, extra terminal events, and unsupported content fail
closed.

## Durable execution sandwich

Every specialist invocation advances one SQLite program counter:

1. `Prepared`
2. `MediaPersisted`
3. `InferenceStarted`
4. `InferenceCompleted`
5. `ArtifactPersisted`
6. `ResultPersisted`

Source media, the full normalized provider receipt, and the normalized output
are encrypted governed artifacts. Their identifiers are deterministic UUIDv5
values derived from `(invocation_id, artifact_kind)`. Repeating an exact write
is idempotent; reusing the identity with different bytes or media type is a
conflict.

The provider request uses `invocation_id` as its request identity. Runtime does
not retry after a stream has opened. Before inference it persists the media and
crosses `InferenceStarted`; after completion it persists the full provider
receipt before advancing the SQLite counter. This leaves an enumerable and
recoverable crash window rather than an ambiguous `Running` state.

`RecoverFromReceipt` can finish every post-inference position without calling
the provider:

- receipt artifact written while SQLite is still `InferenceStarted`;
- receipt committed at `InferenceCompleted`;
- normalized output committed at `ArtifactPersisted`;
- already model-visible `ResultPersisted`.

The recovery path verifies the frozen provider/model, receipt schema,
invocation identity, deterministic artifact locator, output digest, and
monotonic SQLite revision. Missing or conflicting facts stop content-safely.
`NeverReplay` uncertainty remains an operator reconciliation case.

## Observability and Doctor

Runtime observations expose only correlation IDs, success/failure, recovery
source, and counters. They never include media, transcripts, prompts, model
output, or receipts. Operational snapshots distinguish attempted, successful,
failed, and receipt-recovered evaluations.

Doctor derives total, completed, interrupted, and operator-required perception
counts from SQLite. Successful executions therefore remain visible after a
process restart; telemetry is not treated as the recovery source of truth.

## Activation evidence gate

Automatic auxiliary routing stays disabled until a paired benchmark report is
eligible. Each candidate sample must match a primary-only sample on suite,
revision, scenario, topology, workspace, failure point, run ordinal, and exact
primary model. Missing pairs and changed primary models are errors.

Safety is a hard gate: any duplicate effect, invariant violation, user-visible
failure, unsuccessful completion, or Doctor self-application keeps the
candidate disabled. Among safe pairs, the gate uses deterministic paired
statistics:

- minimum sample count;
- median verifier reward gain;
- quality win rate;
- median token increase;
- p95 latency increase.

The policy thresholds are explicit benchmark inputs. A versioned corpus
manifest content-addresses every input and verifier, records provenance and
license, and requires every declared scenario/run exactly once in the
primary-only and candidate arms. Its canonical digest becomes the evidence-set
identity.

An eligible report is converted into a content-free API evidence record; it
still does not activate anything. Registry activation is a separate durable
state machine:

1. an authenticated administrator proposes evidence for one exact Agent
   revision, Agent-definition digest, cognitive role, and provider-qualified
   model;
2. an administrator approves it with an optimistic state revision;
3. an administrator may later revoke it, also with optimistic concurrency.

Only one approved fact may exist for an Agent revision and role. Every state
transition has an append-only actor event, survives restart, and is checked by
Registry health together with evidence and Agent binding digests. Rolling an
Agent head backward or forward cannot carry an approval across revisions.

Versioned production composition loads these approval facts as part of the
immutable Registry closure. Configured roles without approval are removed from
the runnable Agent spec; mismatched or corrupted facts fail composition. This
keeps evaluation, owner authorization, and execution as three distinct
boundaries.
