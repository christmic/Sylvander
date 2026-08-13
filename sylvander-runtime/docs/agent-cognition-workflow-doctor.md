# Agent cognition, workflow, perception, and Doctor

This document fixes four boundaries that become ambiguous once a Runtime can
host multiple first-class Agents and multiple model routes.

## Agent identity is not model identity

An `AgentInstance` is the unit of responsibility. It owns a goal, durable
history view, memory scope, capability envelope, workflow tasks, mailbox,
workspace view, topology relations, approval route, lifecycle, and audit
identity. A model is one replaceable cognitive dependency of that Agent.

One Agent may therefore use several bounded cognitive roles without becoming
a hidden swarm:

- `Primary`: the existing configured model and final model-visible authority;
- `FastDraft`: low-cost draft or routing proposal;
- `Deliberation`: expensive analysis for complex or uncertain work;
- `Critic`: independent review after risk or failure signals;
- `Vision`, `Audio`, and `Document`: perception specialists for native media.

These roles never own tasks, mailboxes, memory, capabilities, topology edges,
approval routes, or workspaces. If a participant needs independent goals,
communication, delegation, governance, or durable ownership, it is a separate
`AgentInstance`, not a cognitive role.

The zero-configuration behavior remains one primary model. Auxiliary roles
must reference the Agent's exact provider/model allowlist, are unique, and
share a hard per-turn call ceiling. Runtime selects roles from content-free
signals in this priority order:

1. required perception modality;
2. high-risk or prior-failure review;
3. high complexity or uncertainty deliberation;
4. low-risk, low-complexity fast drafting.

The policy produces a plan; it does not silently execute extra billable calls.
Activation requires benchmark evidence for quality, cost, latency, and
reliability against the single-model baseline.

The benchmark identity is `role -> exact model`, not a set of distinct model
strings. One exact model may therefore serve both `Primary` and `Deliberation`
to evaluate slow-thinking mode without pretending there are two Agents or two
different models. Primary, auxiliary, and perception calls are reported
separately and must sum to the total call count.

## Perception: built-in port, Skill interpretation

Media transport, decoding, normalization, limits, provenance, and model
capability checks belong to a Runtime-owned perception port. This is the
equivalent of an Agent's eyes and ears: stable infrastructure with governance
and observation, not a prompt convention.

Domain interpretation remains a Skill when it is optional or rapidly evolving:
OCR post-processing, chart analysis, document-specific extraction, medical
image workflows, or UI-specific heuristics. A Skill may consume a normalized
perception artifact but may not bypass media size, retention, capability,
credential, or workspace policy.

Built-in perception and Skills are therefore compositional, not competing
extension mechanisms:

```text
channel media -> governed perception artifact -> cognitive role -> Skill/tool
```

Raw media and derived text remain access-controlled artifacts. Runtime metrics
contain modality, bytes, duration, outcome, latency, and correlation IDs, not
image/audio content.

The current executable boundary is intentionally incremental. Typed image
attachments already become provider-neutral image blocks and require the
primary model's `VISION` capability before dispatch. The content-free
perception planner prefers that native route, otherwise returns only a matching
specialist candidate. It fails closed when transport is absent: configuring an
`Audio` role cannot bypass the fact that typed audio blocks and provider
validation are not yet end to end. Specialist invocation and normalized
artifact persistence remain disabled until benchmark and recovery evidence
land.

## Agent-driven workflow

Runtime provides an environment in which an Agent participates; it does not
reduce the Agent to a scheduler callback. The neutral `manage_workflow` marker
tool lets the model submit a typed intent. Runtime injects the true Session and
Agent identity, resolves current membership/topology, applies policy, derives
revision fences, and commits to SQLite.

Current actions are deliberately small:

- create one self-owned durable task with token and handoff ceilings;
- transition an assigned task through running, blocked, review, and terminal
  states while reporting cumulative token consumption.

The service also supports moderator-created work for a governed descendant.
An ordinary Agent cannot assign work across an unrelated ownership branch.
Existing message, wait, progress, handoff, arbitration, and worktree services
remain the mechanisms for collaboration. Future tool actions should call those
services rather than duplicate their state machines.

Long-running work becomes a durable task plus a mailbox-bound Agent turn.
`start_background_task` now creates those facts deterministically before
dispatch; Runtime recovers missing wakes/outbox delivery and fences task
execution leases after restart.

## Soft recovery

Soft recovery means preserving truthful progress and a useful user experience;
it does not mean hiding uncertainty or blindly retrying effects.

Runtime follows this order after interruption:

1. read the durable turn, model, tool, mailbox, and workflow positions;
2. resume a completed fact without repeating its effect;
3. retry only when the independent recovery policy permits the same stable ID;
4. ask a receipt or journal to reconcile an uncertain committed effect;
5. convert an unrecoverable sub-operation into a model-visible structured
   observation and let the primary Agent replan;
6. wake the moderator for cross-Agent uncertainty or hard governance findings;
7. require a human only when neither deterministic recovery nor bounded AI
   reconciliation can preserve correctness.

A lost provider stream is not itself a failed user turn. If the model ledger
proves no accepted response, Runtime may retry under the provider policy. If
acceptance is uncertain and no receipt protocol exists, Runtime records that
fact and asks the Agent to respond or replan without asserting that the model
call failed or succeeded. Billing uncertainty is observable separately.

## Observation stream and Doctor

`AgentEvent` remains truth for the smallest turn machine. `RuntimeEvent` is
truth for composition: Session admission, persistence boundaries, model/tool
recovery, workflow transitions, coordination, moderation, workspace leases,
and subsystem health. Durable rows are the audit source; the bounded event bus
and JSONL debug projection are observation delivery mechanisms.

Doctor is a consumer of content-safe Runtime facts, not a privileged mutation
hook. It may:

- diagnose lifecycle defects, recovery frequency, stagnation, token waste,
  latency regressions, and capability mismatch;
- correlate a symptom with exact immutable Agent/model/policy revisions;
- propose a controlled experiment and expected invariant;
- evaluate the candidate against replayable fixtures and external benchmarks;
- request a governed new revision.

Doctor may not alter the current running revision, erase adverse evidence,
weaken recovery policy, grant capabilities, or auto-promote a candidate. This
keeps self-improvement reversible: observe, hypothesize, experiment, compare,
approve, activate, and retain rollback.

Agents now receive an `inspect_runtime` control tool backed by a read-only
Doctor port. It exposes only bounded counts and attention state derived from
the caller's durable Session membership, topology, task graph, workspace,
arbitration, and recovery ledgers. It has no mutation operation, and forged
non-members fail before any report is returned. This lets an Agent replan from
its environment while revision activation stays with governed Runtime APIs.

## Runtime benchmark portfolio

Runtime evaluation is a scenario matrix, not another model leaderboard:

```text
scenario × topology × workspace mode × failure point × cognition profile
× provider/model set × repetition
```

Every cell records correctness, durable invariant violations, user-visible
failure rate, recovery decision, duplicate-effect count, useful completion,
latency, tokens, model calls, handoffs, messages, moderation, and worktree
conflicts. Required local fixture families are:

- crash injection at every model/tool/workflow/mailbox boundary;
- duplicate delivery and stale lease/fencing attempts;
- DAG cycles, wait SCCs, stagnant progress, handoff ping-pong, and moderator
  replacement;
- concurrent isolated worktrees, stale merge bases, and semantic conflicts;
- single-model versus fast/slow, primary/critic, and modality-role ablations;
- Doctor diagnosis and candidate-revision rollback.

External suites remain verifier-owned and version pinned. Relevant primary
sources are:

- [Harbor](https://github.com/harbor-framework/harbor) and
  [Terminal-Bench](https://github.com/harbor-framework/terminal-bench) for
  isolated terminal tasks and trajectory interchange;
- [SWE-bench](https://github.com/SWE-bench/SWE-bench) for repository issue
  resolution;
- [AgentBench](https://github.com/THUDM/AgentBench) for containerized OS,
  database, knowledge, shopping, and embodied task families;
- [tau3-bench](https://github.com/sierra-research/tau2-bench) for policy-bound
  tool/user interaction, knowledge retrieval, and native voice;
- [OSWorld](https://github.com/xlang-ai/OSWorld) and
  [VisualAgentBench](https://github.com/THUDM/VisualAgentBench) for multimodal
  perception and computer use;
- [WebArena](https://github.com/web-arena-x/webarena) for reproducible web
  interaction;
- [MultiAgentBench/MARBLE](https://github.com/MultiagentBench/MARBLE) for
  collaboration and competition scenarios;
- [GAIA](https://huggingface.co/gaia-benchmark) for heterogeneous assistant
  tasks requiring tools and multiple modalities.

External scores cannot prove Runtime crash correctness. Conversely, synthetic
fault fixtures cannot prove useful task completion. Release evidence requires
both layers and retains every repetition rather than averaging incompatible
suite rewards into one number.
