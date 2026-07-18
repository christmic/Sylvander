# Current release baseline and deployment gates

Status date: 2026-07-18

This record closes the implemented local product scope and separates
deterministic default gates from deployment-specific acceptance journeys. The
active
[`production-expansion-checklist.md`](production-expansion-checklist.md) is
fully checked. The same tracked implementation state passed the complete root
workspace, nested Token9, security, performance, clean-room, and deterministic
Ghostty matrices described below. The unified actor-aware capability router is
also covered by focused Worker/Guardian, invocation, approval, artifact, and
learning-opt-out tests.

## Supported release scope

The supported product is a server-owned Agent runtime with the terminal client
as its primary interactive surface. It includes durable sessions and memory,
runtime-selected Agents and models, configurable prompts and workspaces, local
and isolated-worktree coding, OpenSSH execution and remote Git worktrees,
restricted OCI container/sandbox execution,
typed approvals and questions, Unix/HTTP/WebSocket channels, multi-instance
DingTalk, Telegram, and WeChat Work adapters, MCP/skills/hooks/extensions,
typed turn context, isolated Worker/Guardian curation, governed evidence and
artifacts, explicit governed-memory confirmation, renewable credential leases
with a separate content-safe operation ledger, operational diagnostics, and
evidence-driven improvement experiments with a local human-gated
administrator command.

Local execution remains the zero-external-dependency baseline. Configured SSH
targets use strict host-key verification, bounded OpenSSH control reuse,
location-transparent tools, remote process-group cancellation, and durable
remote worktree create/review/accept/discard/reconciliation. The credentialed
real-SSH journey is opt-in and must pass on each deployment before that target
is advertised. The development acceptance host passed the disposable
local-SSH execution, cancellation, restart, review, accept, and discard journey
on 2026-07-18. A native interactive SSH terminal and native tmux integration
are not advertised; terminal reflow is verified against `screen-256color`, and
a deployment that depends on a real tmux process must supply that executable
and run the opt-in journey.

## Reproducible release gates

Run these commands at the repository root:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked
cargo build --workspace --release --locked
./scripts/verify-docs.sh
./scripts/security-verify.sh
./scripts/performance-verify.sh
./scripts/clean-room-verify.sh

(
  cd sylvander-token9
  cargo fmt --all -- --check
  cargo test --workspace --locked
  cargo clippy --workspace --all-targets --locked -- -D warnings
  RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked
)
```

Release evidence is valid only when every command above passes against the same
tracked commit. The clean-room gate exports that `HEAD` into a new directory,
installs locked offline release binaries, starts the installed server from a
newly generated production configuration, observes its Unix socket and durable
databases, verifies the installed TUI, and requires a clean signal-driven
shutdown.

The nested token9 workspace is first-party code but intentionally does not
participate in the root Cargo workspace. Its independent format, full-test,
strict-Clippy, and warning-denied Rustdoc block above is therefore required; a
green root workspace cannot stand in for it. `verify-docs.sh` requires one
indexed module boundary for all 16 first-party Cargo packages and rejects
broken relative links in the maintained documentation set.

The real-client gate compiles the TUI and drives it through a pseudo-terminal.
It covers protocol negotiation, keyboard submission, streamed output, AskUser,
approval rejection, interrupt, resize, reconnect, persisted SQLite resume, and
colliding multi-client isolation. The approval journey additionally proves
that a rejected write never executes. TUI unit, E2E, PTY, real-Agent PTY, and
visual snapshot suites pass together.

The local self-improvement administrator gate invokes the compiled
`sylvander-improve` binary for proposal creation and transitions plus
experiment start, evaluation, acceptance, observation, and rollback. Its two
temporary Git repositories prove both a successful observed merge and a clean
human-directed revert, then reopen the durable store to verify terminal state.
This is not evidence of an automatic or remote production rollout.

The desktop-host gate is recorded in
[`ghostty-release-verification.md`](ghostty-release-verification.md). It
requires the full exact-v5 Swift suite, a clean Release universal bundled
helper with strict local signature verification, and a real Unix-backed
`ReleaseLocal` lifecycle with active/inactive transparency plus TrueColor
inspection. Those deterministic local checks close the implementation gate.
A consistently Developer ID-signed Release lifecycle, notarization, and
stapling are external distribution prerequisites; `ReleaseLocal` is not
evidence that those credentialed deployment checks passed.

The security gate reports no RustSec vulnerability in locked dependencies and
covers malformed protocol input, path and command-argument injection,
cross-owner isolation, redaction, tracked-secret scanning, and learned-data
deletion. The performance gate completes the locked release build and verifies
bounded concurrent delivery, parallel tools, long transcripts, large local
workspaces, bursts, and executor ceilings within the documented budgets.

No critical or high-severity defect is recorded in the supported scope. The
closure matrix completed with a clean worktree and no generated snapshot
drift.

## Residual risk and non-claims

- Credentialed live-provider and live external-channel tests are opt-in. The
  default release gate uses deterministic local fakes and contract tests.
  Provider and channel credential smoke journeys were unavailable in the
  current development environment and remain deployment prerequisites wherever
  those adapters are enabled.
- Docker or Podman daemon availability is environment-dependent. OCI command
  composition, restrictions, cleanup, and host-backed coding journeys are
  deterministic. No OCI daemon was available for the current development
  evidence, so the real-daemon smoke remains a deployment prerequisite.
- The configured registry mirror does not expose Cargo's yanked-package
  metadata, so the repeatable audit uses `cargo audit --no-yanked`. RustSec
  vulnerability checks still cover the complete locked dependency graph.
- The deterministic default gate cannot certify a deployment's SSH daemon,
  credentials, host keys, network, or remote repository. The opt-in real-SSH
  journey is the required deployment evidence even though the disposable local
  journey passed. A native tmux executable was unavailable for the current
  evidence; native interactive SSH-terminal and tmux process integration remain
  unadvertised unless a deployment supplies and passes their acceptance
  journey.
- Local ad-hoc Release signing verifies bundle structure, universal helper,
  and nested code signatures. It does not provide the Team ID needed for
  hardened library validation of the embedded Sparkle framework; local
  lifecycle/visual inspection therefore uses the separately entitled
  `ReleaseLocal` configuration. Developer ID Release launch, distribution
  signing, notarization, and stapling still require the Apple identities and
  credentials documented in
  [`sylvander-ghostty-architecture.md`](sylvander-ghostty-architecture.md).

These are bounded environmental or explicit non-claims, not hidden fallback
behavior.
