# Channel instance supervision

The Runtime owns channels as configured instances, not as transport kinds. A
registration contains:

- a stable `instance_id`;
- one adapter implementing the common `Channel` contract;
- a bounded restart policy.

The server derives the registration from `channels[]`. The adapter still owns
transport details, authentication, inbound normalization, and outbound
delivery. The Runtime owns lifecycle, health, restart, and drain.

## Lifecycle

1. Every instance starts in `Starting`.
2. The adapter binds its ingress and calls `ChannelContext::mark_ready()`.
3. Startup fails if the adapter exits first or does not become ready within
   five seconds. Already-ready instances are drained during rollback.
4. An unexpected exit after readiness changes the instance to `Restarting`.
5. The Runtime retries with bounded exponential backoff.
6. Exhausting `max_restart_attempts` changes the instance to `Failed` and
   reports its stable instance ID to the server.
7. Runtime shutdown sends one cooperative signal shared by all attempts,
   allows five seconds to drain, then aborts a stuck task.

`Runtime::channel_health()` returns content-free snapshots containing instance
ID, adapter kind, state, and restart count.

## Configuration

Each channel may override the defaults:

```toml
[[channels]]
id = "telegram-primary"
default_agent = "sylvander"

[channels.supervision]
max_restart_attempts = 5
initial_backoff_ms = 250
max_backoff_ms = 5000
```

The restart budget is at most 20. Initial backoff is 10–60000 ms. Maximum
backoff must be greater than or equal to initial backoff and no more than
300000 ms. Zero restart attempts means an unexpected post-readiness exit is
reported immediately.

## Isolation

- External identity includes transport, instance ID, and principal.
- Session ownership and outbound mapping include the instance ID.
- DingTalk, Telegram, and WeChat reject bounded, expiring replay keys.
- Outbound adapters subscribe only to their configured Agent and then enforce
  instance-owned session mapping before delivery.
- Adapter bind, serve, or subscription errors return to the supervisor; they
  do not panic the process.

The TUI is a single-session Unix client. Multi-session presentation belongs to
the Ghostty host and does not change this channel lifecycle contract.

## Executable acceptance

`sylvander-server/tests/channel_instances.rs` starts the production server
binary from a real `ServerConfig` containing two enabled HTTP adapters. The
journey requires both listeners to report the same two-instance Runtime
health, proves that each instance accepts only its own secret lease, checks
that credential operations are recorded under disjoint
`channel_instance/<id>` audit subjects, then sends `SIGINT` and requires both
instances to stop before the process exits. The test also rejects either
secret appearing in lifecycle logs. This is the production-composition
acceptance path; in-memory `Channel` doubles remain appropriate only for
focused supervisor-state tests.
