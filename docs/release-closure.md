# Local-first release closure

Status date: 2026-07-16

This record closes the current Sylvander implementation checklist. It describes
the capability actually shipped and keeps future remote work out of the local
release claim.

## Supported release scope

The supported product is a server-owned Agent runtime with the terminal client
as its primary interactive surface. It includes durable sessions and memory,
runtime-selected Agents and models, configurable prompts and workspaces, local
and isolated-worktree coding, restricted OCI container/sandbox execution,
typed approvals and questions, Unix/HTTP/WebSocket channels, multi-instance
DingTalk and Telegram adapters, MCP/skills/hooks/extensions, operational
diagnostics, and evidence-driven improvement experiments.

Local execution is the release baseline. SSH execution, remote worktrees, and
SSH terminal verification are one explicitly deferred future track and are not
advertised. Native tmux integration is also outside this release; terminal
reflow is verified against its `screen-256color` surface.

## Reproducible release gates

Run these commands at the repository root:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --locked
./scripts/security-verify.sh
./scripts/performance-verify.sh
./scripts/clean-room-verify.sh
```

The 2026-07-16 candidate passed every command. The clean-room gate exports
tracked `HEAD` into a new directory, installs locked offline release binaries,
starts the installed server from a newly generated production configuration,
observes its Unix socket and durable databases, verifies the installed TUI, and
requires a clean signal-driven shutdown.

The real-client gate compiles the TUI and drives it through a pseudo-terminal.
It covers protocol negotiation, keyboard submission, streamed output, AskUser,
approval rejection, interrupt, resize, reconnect, persisted SQLite resume, and
colliding multi-client isolation. The approval journey additionally proves
that a rejected write never executes. TUI unit, E2E, PTY, real-Agent PTY, and
visual snapshot suites pass together.

The security gate reports no RustSec vulnerability in locked dependencies and
covers malformed protocol input, path and command-argument injection,
cross-owner isolation, redaction, tracked-secret scanning, and learned-data
deletion. The performance gate completes the locked release build and verifies
bounded concurrent delivery, parallel tools, long transcripts, large local
workspaces, bursts, and executor ceilings within the documented budgets.

No critical or high-severity defect is known in the supported scope.

## Residual risk and non-claims

- Credentialed live-provider and live external-channel tests are opt-in. The
  default release gate uses deterministic local fakes and contract tests; tests
  requiring private credentials remain explicitly ignored when those
  credentials are absent.
- Docker or Podman daemon availability is environment-dependent. OCI command
  composition, restrictions, cleanup, and host-backed coding journeys are
  deterministic; an operator should run the documented daemon smoke test on
  each deployment host.
- The configured registry mirror does not expose Cargo's yanked-package
  metadata, so the repeatable audit uses `cargo audit --no-yanked`. RustSec
  vulnerability checks still cover the complete locked dependency graph.
- SSH execution, SSH terminal behavior, remote worktrees, and native tmux
  process integration are future release tracks. No fallback silently claims
  these capabilities.

These are bounded environmental or future-scope risks, not unchecked items in
the local-first release.
