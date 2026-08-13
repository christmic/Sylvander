# Agent benchmark case catalog

## How to read this catalog

These are regression cases, not a substitute for full release suites. Each
case keeps its upstream task identifier and immutable dataset revision. “Pass”
means the task-owned verifier accepts the final environment; it does not mean
all capabilities in that family are solved.

The fixed matrix repeats each case three times for every provider/protocol/model
deployment. The current matrix contains 15 Terminal-Bench and 12 SWE-bench
Verified tasks. τ³ text and AgentBench FC become executable entries only after
the production external-tool boundary exists; their intended coverage is
listed so missing capability remains visible.

## Terminal-Bench 2.0 regression cases

Dataset revision: `69671fbaac6d67a7ef0dfec016cc38a64ef7a77c`.

| Task | What the Agent must do | Capability signal |
| --- | --- | --- |
| `build-cython-ext` | Repair and build pyknotid Cython extensions against modern NumPy | native build/debug/dependency work |
| `cancel-async-tasks` | Implement bounded-concurrency async jobs with correct cancellation | concurrency semantics and focused coding |
| `configure-git-webserver` | Configure SSH Git push to deploy content through a port-8080 web server | service configuration and end-to-end Git |
| `custom-memory-heap-crash` | Fix a release-only C++ crash while changing only `user.cpp` | native debugging under file constraints |
| `db-wal-recovery` | Repair/recover a SQLite WAL and emit all records as JSON | binary/data recovery and validation |
| `fix-code-vulnerability` | Identify the CWE in a Bottle repository and patch it | security analysis and repository editing |
| `git-multibranch` | Serve main/dev Git branches through separate HTTPS paths | multi-service Git, SSH, TLS and Nginx setup |
| `gpt2-codegolf` | Write a dependency-free, size-limited C GPT-2 checkpoint sampler | extreme systems implementation and constraints |
| `kv-store-grpc` | Define protobuf and implement a Python gRPC numeric KV server | API/schema generation and service execution |
| `large-scale-text-editing` | Transform one million CSV rows using three constrained Vim macros | exact bulk editing under command restrictions |
| `portfolio-optimization` | Complete a C/Python implementation matching a numerical baseline faster | numerical correctness and optimization |
| `protein-assembly` | Design a DHFR fusion satisfying spectral and binding constraints | scientific research reasoning and artifact creation |
| `pytorch-model-recovery` | Infer a model architecture from weights/data and reconstruct it | ML forensics and executable validation |
| `query-optimize` | Rewrite an OEWN SQLite query for speed with identical output | SQL reasoning and performance |
| `sanitize-git-repo` | Find and replace API keys throughout a Git repository | secret discovery, safe editing and coverage |

For L1, start with `cancel-async-tasks`, `large-scale-text-editing` and
`sanitize-git-repo`. They cover coding, constrained tool use and security/Git
without intentionally relying on an external HTTP test service.

## SWE-bench Verified regression cases

Dataset revision: `86723674f04e4209ac479d0fb75d9d9f44b4377e`.

| Task | Reported issue the patch must resolve | Capability signal |
| --- | --- | --- |
| `astropy__astropy-13977` | incompatible Quantity ufunc inputs should permit reflected operations | numerical protocol and dispatch semantics |
| `django__django-15098` | i18n locales containing both script and region fail routing | framework parsing and standards handling |
| `matplotlib__matplotlib-23476` | unpickling repeatedly doubles figure DPI on M1 Mac | platform-sensitive state restoration |
| `mwaskom__seaborn-3187` | legends for large numeric ranges lose formatter offsets | visualization formatting correctness |
| `pallets__flask-5014` | Blueprints accept an invalid empty name | API validation and regression testing |
| `psf__requests-2317` | byte-string HTTP methods become literal `b'GET'` strings | compatibility conversion and HTTP behavior |
| `pydata__xarray-3095` | deep copy casts Unicode indices to object dtype | data model, dtype and copy semantics |
| `pylint-dev__pylint-4604` | type-comment module use is falsely reported unused | static-analysis AST/name tracking |
| `pytest-dev__pytest-5809` | pastebin submission uses a lexer that triggers HTTP 400 | request payload construction and integration behavior |
| `scikit-learn__scikit-learn-12585` | estimator classes cannot be cloned as parameters | object cloning and estimator contracts |
| `sphinx-doc__sphinx-8593` | autodoc ignores `:meta public:` on variables | parser/directive metadata behavior |
| `sympy__sympy-13852` | known polylogarithm values do not expand symbolically | symbolic transformation and exact mathematics |

Each native arm64 rebuild must pass its gold patch before an Agent result is
eligible. Tasks whose verifier requires an unstable public service need a
pinned local replacement proven equivalent, or remain infrastructure-blocked.

## τ³-bench text coverage

The required text portfolio will sample retail, airline, telecom and knowledge
domains. Cases exercise multi-turn user clarification, policy constraints,
domain-owned tool calls, tool-result continuation and final task reward. A
direct model adapter is forbidden because it would not measure Sylvander's
Agent loop. Exact task IDs and dataset revision are added when the external
tool suspend/resume production capability lands.

## AgentBench FC coverage

The required function-calling portfolio will sample operating-system,
database, knowledge-graph, WebShop and ALFWorld environments. Cases exercise
tool selection, JSON argument validity, observation use, multi-step planning
and per-domain success. Exact task IDs and revision are pinned only when the
same external-tool boundary can execute them through the real Sylvander Agent.

OSWorld and WebArena are intentionally outside the current gate.
