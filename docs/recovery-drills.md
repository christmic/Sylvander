# Recovery drills

Sylvander is pre-release and follows the latest-interface policy. Recovery
does not imply automatic compatibility with an old schema. Unless an approved
task names a source version and transition, old, future, unmanaged, or damaged
schemas fail startup without repair or fallback.

Run these drills from a clean checkout before a local production release.

## Configuration and registry

The registry bootstrap and activation tests prove:

- the initial projection is deterministic and retry-safe;
- a failed candidate composition does not move an active head;
- an interrupted administration intent is completed exactly once;
- provider/model/credential revisions and active heads survive restart;
- damaged or dual-owned schema state fails closed;
- historical sessions retain exact Agent/provider/model revision pins.

```sh
cargo test -p sylvander-runtime registry_bootstrap
cargo test -p sylvander-runtime registry_admin
cargo test -p sylvander-runtime registry_composition
cargo test -p sylvander-runtime agent_admin_runtime_v3
```

## Session and run recovery

Runtime startup reloads persistent sessions before accepting channels.
Evidence startup marks in-flight runs, turns, and steps interrupted instead of
presenting them as successful. Shutdown drains queued evidence, closes active
turns, and records the run terminal state.

```sh
cargo test -p sylvander-runtime boot_loads_persistent_sessions
cargo test -p sylvander-runtime configured_boot_restores_database_session
cargo test -p sylvander-runtime evidence::tests::reopening_marks_inflight_records_interrupted
```

## Channel restart

Each configured channel instance has bounded restart/backoff state. A failure
before readiness fails startup and drains already-ready peers. A failure after
readiness is isolated, reflected in per-instance health, retried within policy,
and reported terminally when exhausted.

```sh
cargo test -p sylvander-runtime channel_exit_before_readiness_fails_startup
cargo test -p sylvander-runtime ready_channel_is_restarted_and_health_is_instance_scoped
cargo test -p sylvander-runtime shutdown_cancels_owned_channel_tasks_before_returning
```

## Worktree and executor recovery

Runtime persists one lease manifest per isolated local coding session. On
restart it validates every active lease against durable session state, removes
deleted-session leases, and removes a worktree left before its manifest
commit. Accepted changes use explicit merge commits; observed self-change
regressions can revert only while that merge remains current.

```sh
cargo test -p sylvander-runtime git_worktree
cargo test -p sylvander-runtime coding_tool_review_and_resume_survive_runtime_restart
cargo test -p sylvander-runtime container_coding_session_runs_in_worktree_and_survives_restart
cargo test -p sylvander-runtime reviewed_local_experiment_rolls_back_observed_regression
```

## Memory backup and restore

Production relationship memory uses the exact current schema, an authenticated
external epoch/root anchor, verified backup pairs, and bounded rotation.
Restore is offline and accepts only a signed backup matching the currently
anchored epoch. Temporary/orphan artifacts do not count as a backup.

```sh
cargo test -p sylvander-agent memory_sqlite::backup
cargo test -p sylvander-runtime memory_maintenance
cargo test -p sylvander-runtime production_memory_catch_up_is_bounded_restart_safe_and_idempotent
```

Never delete the database, anchor, or lease directory to make a failed drill
pass. Preserve the failed artifact, restore the prior verified pair, and record
the content-safe error and exact commit used for the drill.

## Release recovery gate

The recovery gate is:

```sh
cargo test -p sylvander-runtime --lib
cargo test -p sylvander-agent --lib
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
```

Any failed drill blocks release. A waived external dependency, such as an OCI
daemon unavailable on the current host, must be recorded as residual risk; it
must not be represented as a passing real-environment drill.
