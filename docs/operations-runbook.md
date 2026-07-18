# Sylvander operations runbook

This runbook covers the current single-server deployment, including configured
local, SSH, container, and managed-sandbox execution targets. Native interactive
SSH terminals are not part of the TUI contract; Agent tools access remote
workspaces through the location-transparent executor.

## Health and readiness

An enabled HTTP channel exposes three unauthenticated, content-free operations:

- `GET /health` returns the Runtime dependency snapshot. HTTP 200 means all
  configured Agents and supervised channels are ready; HTTP 503 means degraded
  or unavailable.
- `GET /ready` returns only `{"ready":true|false}` with the same 200/503
  contract. Use this for process managers and load balancers.
- `GET /metrics` returns Prometheus text for Agent/session/channel counts,
  bounded message-bus capacity/subscribers, successful publishes, and
  backpressure rejections.

The snapshot never contains prompts, messages, tool inputs/results, external
principal IDs, credentials, paths, or memory content.

Recommended probes:

```sh
curl --fail http://127.0.0.1:8080/ready
curl --fail http://127.0.0.1:8080/health
curl --fail http://127.0.0.1:8080/metrics
```

Readiness must not be replaced with process liveness. A running process whose
Agent failed to start or whose supervised channel is terminal is not ready.

## Metrics and alerts

The core alert conditions are:

- `sylvander_ready == 0` for two consecutive probe intervals;
- any increase in `sylvander_bus_backpressure_rejections_total`;
- ready channel count lower than total channel count;
- an unexpected drop in Agent count;
- sustained session growth without corresponding completed evidence runs.

Backpressure is explicit. Each in-process subscription has a fixed capacity of
256 messages. A publish checks every matching subscriber before delivery. If
any live subscriber is full, the entire publish is rejected before partial
delivery. Callers receive an error and must not assume the message was queued.

Ingress quotas are configured under `server.boundary`:

```toml
[server.boundary]
max_request_bytes = 1048576
requests_per_minute = 240
```

The rate window is isolated by channel instance and authenticated principal;
unauthenticated failures share a bounded anonymous window.

## Structured tracing

`RUST_LOG` selects the standard tracing filter. Set
`SYLVANDER_LOG_FORMAT=json` to emit one flattened JSON object per event for a
collector. Any other value uses the human-readable formatter.

```sh
RUST_LOG=sylvander_runtime=info,sylvander_agent=info \
SYLVANDER_LOG_FORMAT=json \
./target/release/sylvander
```

Logs are operational events, not transcript export. Credentials and raw
provider secrets must never be fields. Run evidence and administration audit
use their own durable, content-governed stores.

## Incident triage

1. Check `/ready`; if 503, inspect `/health` for Agent/channel counts.
2. Check whether backpressure rejections are increasing. If so, identify the
   stalled channel or client before retrying submissions.
3. Inspect structured logs for the stable channel instance ID and restart
   count. Do not use display names as identity.
4. For an interrupted coding session, inspect the durable session and
   worktree lease. Runtime startup reconciles active leases and removes
   deleted-session or pre-manifest orphans.
5. For memory startup failure, do not delete or rewrite the database. Verify
   the configured anchor and use the signed backup/restore procedure.
6. For a self-change regression, verify the signed observation bundle,
   recorded merge commit, and rollback commit. Automatic revert is allowed
   only while the reviewed merge is still the current source commit.

For an SSH target failure, verify the deployment-owned `known_hosts` file,
identity-path secret, private control-socket parent, remote workspace, and
remote worktree root. Never replace strict host-key checking with interactive
learning. Before enabling the target for users, run the ignored
`real_ssh_executor_worktree_restart_review_accept_and_cancel` acceptance test
against a disposable SSH daemon and repository. It verifies bounded execution,
remote descendant cancellation, durable worktree restart reconciliation,
review, and acceptance.

## Shutdown

Send SIGINT once. Runtime stops accepting channel work, cooperatively drains
channel tasks, stops Agent workers, completes the active Guardian curation
pass, closes active evidence turns, publishes the final maintenance state, and
returns an error if any owned component failed to drain. Do not send a second
signal unless the configured external supervisor has exceeded its termination
deadline.
