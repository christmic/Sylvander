#!/bin/sh
set -eu

cd "$(git rev-parse --show-toplevel)"

run_budget() {
  budget="$1"
  label="$2"
  shift 2
  started="$(date +%s)"
  "$@"
  elapsed="$(( $(date +%s) - started ))"
  if [ "$elapsed" -gt "$budget" ]; then
    echo "$label exceeded ${budget}s budget (${elapsed}s)" >&2
    exit 1
  fi
  echo "$label passed in ${elapsed}s (budget ${budget}s)"
}

cargo build --workspace --release --locked
# Prewarm the exact feature sets used below. Workspace-wide feature unification
# can produce different test binaries from an individual package invocation;
# compilation time is not part of an interaction latency budget.
export CARGO_TARGET_DIR="$PWD/target/performance"
cargo test -p sylvander-protocol --lib --no-run
cargo test -p sylvander-agent --lib --no-run
cargo test -p sylvander-agent --test simple_run --no-run
cargo test -p sylvander-tui --lib --no-run
cargo test -p sylvander-runtime --lib --no-run

run_budget 10 "message-bus burst" \
  cargo test -p sylvander-protocol --lib concurrent_publish_burst_delivers_every_message_within_capacity
run_budget 10 "large workspace bounds" \
  cargo test -p sylvander-agent --lib large_local_workspace_queries_stop_at_their_result_budget
run_budget 10 "concurrent tool scheduling" \
  cargo test -p sylvander-agent --test simple_run ordinary_tool_batch_starts_and_executes_concurrently
run_budget 10 "tool progress burst" \
  cargo test -p sylvander-agent --test simple_run tool_progress_is_bounded
run_budget 10 "long TUI transcript retention" \
  cargo test -p sylvander-tui --lib transcript_window_is_bounded_by_entries_and_bytes
run_budget 10 "TUI input flood" \
  cargo test -p sylvander-tui --lib redraw_flood_is_bounded_without_dropping_a_later_key
run_budget 10 "TUI service backpressure" \
  cargo test -p sylvander-tui --lib socket_event_queue_applies_backpressure_at_its_capacity
run_budget 10 "container resource ceilings" \
  cargo test -p sylvander-runtime --lib container_and_sandbox_resource_limits_are_bounded

echo "local performance verification passed"
