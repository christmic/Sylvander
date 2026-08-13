# Runtime benchmark corpus

Last verified: 2026-08-14

Runtime evaluation must separate three questions:

1. Did the Agent solve the external task?
2. Did Runtime preserve safety, exact-once, budget, topology, and workspace invariants?
3. Did recovery hide infrastructure failure from the user without inventing success?

An external benchmark supplies the first signal. Sylvander's deterministic
fault harness and observation stream supply the latter two. A high task reward
never cancels an invariant violation, duplicate effect, or user-visible failure.

## Adapter candidates

| Corpus | Runtime surface | Why it belongs | Adapter posture |
|---|---|---|---|
| [SWE-bench](https://github.com/swe-bench/SWE-bench) | coding, tools, worktrees | Real repository issues with executable verification | First coding adapter; run isolated worktrees and inject crashes around edits/tests |
| [GAIA](https://arxiv.org/abs/2311.12983) | general assistant, web, multimodal | Multi-step questions requiring reasoning, browsing, tools, and multiple modalities | Import public split metadata; keep answers/scoring outside Runtime |
| [OSWorld](https://github.com/xlang-ai/OSWorld) | vision, computer use, long horizon | Real desktop applications and execution-based evaluation | VM/container adapter; never grant host control to the benchmark process |
| [WebArena](https://github.com/web-arena-x/webarena) | browser, stateful web workflows | Self-hosted websites with executable task evaluators | Isolated network fixture; capture task state rather than page content in Runtime evidence |
| [AgentBench](https://github.com/THUDM/AgentBench) | OS, database, knowledge graph, shopping | Diverse interactive environments and multi-turn decisions | Use containerized environments; map each environment to an explicit capability profile |
| [BFCL](https://github.com/ShishirPatil/gorilla/tree/main/berkeley-function-call-leaderboard) | tool selection and multi-turn calls | Executable function calling, relevance, parallel and multi-call cases | Agent-kernel baseline plus Runtime replay/idempotency variants |
| [tau2-bench](https://github.com/sierra-research/tau2-bench) | policy-constrained tool workflows | Stateful user/tool interaction and rule adherence | Add crash/retry points at every tool effect boundary |
| [MultiAgentBench](https://github.com/MultiagentBench/MARBLE) | collaboration and competition | Explicit multi-Agent coordination scenarios | Compare single Agent, fork tree, peer mesh, and moderator swarm under equal budgets |
| [PaperBench](https://openai.com/index/paperbench/) | long-running research workflow | Hierarchical rubrics over long-horizon engineering work | Later adapter; useful for durable tasks, checkpoints, evidence, and restart continuity |
| [TUA-Bench](https://tuabench.ai/) | terminal use | Deterministic setup and execution-based terminal grading | Add after the terminal sandbox adapter is reproducible in CI |

Dataset code and task data are not copied into this repository. Every adapter
must pin an upstream revision, record license and access requirements, and hash
the imported split manifest. Gated or changing datasets remain explicit
unsupported cells rather than silently shrinking a run.

## Sylvander-native fault suites

External corpora do not exercise Runtime's hardest invariants, so every adapter
is crossed with deterministic local scenarios:

- crash after intent but before model/tool effect;
- crash after effect start with safe, receipt-recoverable, and never-replay tools;
- crash after effect commit but before model-visible result persistence;
- task lease expiry followed by a stale executor commit;
- interrupted background task creation between task and mailbox outbox writes;
- mailbox wake loss, claim expiry, delivered-turn recovery, and dead-letter bounds;
- simultaneous peer handoff, wait-cycle, ping-pong, stagnation, and moderator arbitration;
- concurrent edits in isolated worktrees, target branch advance, merge conflict, and rollback;
- primary-only versus fast/slow, primary/critic, and perception-specialist cognition under the same total token budget;
- observation/Doctor regression detection with a shadow proposal that is never self-applied.

## Required reporting

Each cell records exact suite revision, scenario ID, model identities, topology,
workspace isolation, failure point, cognition profile, repetition, task reward,
useful completion, invariant violations, duplicate effects, user-visible
failures, recovery, latency, token/model/tool/message/handoff counts, moderator
interventions, and workspace conflicts.

Release comparison uses paired coordinates and reports the full matrix. The
minimum release gate is zero duplicate effects and zero invariant violations;
task reward, latency, and token cost are optimization objectives, not safety
waivers.
