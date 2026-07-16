#!/bin/sh
set -eu

cd "$(git rev-parse --show-toplevel)"

secret_pattern='(sk-[A-Za-z0-9_-]{20,}|AKIA[0-9A-Z]{16}|-----BEGIN (RSA |EC |OPENSSH )?PRIVATE KEY-----|gh[pousr]_[A-Za-z0-9]{20,})'
secret_hits="$(
  git grep -n -I -E "$secret_pattern" -- ':!Cargo.lock' |
    grep -v '^sylvander-tui/src/tool_presenter.rs:1151:' || true
)"
if [ -n "$secret_hits" ]; then
  printf '%s\n' "$secret_hits" >&2
  echo "tracked high-confidence secret pattern detected" >&2
  exit 1
fi

cargo metadata --locked --no-deps --format-version 1 >/dev/null
if command -v cargo-audit >/dev/null 2>&1; then
  audit_command="$(command -v cargo-audit)"
elif [ -x "$HOME/.cargo/bin/cargo-audit" ]; then
  audit_command="$HOME/.cargo/bin/cargo-audit"
else
  echo "cargo-audit is required for release security verification" >&2
  exit 1
fi
GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=http.proxy GIT_CONFIG_VALUE_0= \
  "$audit_command" audit --no-yanked

cargo test -p sylvander-protocol --lib mutated_client_frames_are_total_and_strict_shapes_fail_closed
cargo test -p sylvander-agent --lib read_path_outside_workdir_rejected
cargo test -p sylvander-agent --lib diff_rejects_shell_arguments_and_parent_paths
cargo test -p sylvander-agent --lib relationship_operations_isolate_user_and_agent
cargo test -p sylvander-agent --lib maintenance_forgets_the_complete_chain_with_content_safe_audit
cargo test -p sylvander-runtime --lib restart_restores_exact_owner_profile_and_isolates_other_users
cargo test -p sylvander-runtime --lib production_memory_isolates_same_user_across_agent_owners
cargo test -p sylvander-channel-unix --lib credential_create_round_trip_returns_only_redacted_view
cargo test -p sylvander-channel-unix --lib socket_permissions_and_live_events_are_isolated_between_clients
cargo test -p sylvander-tui --lib secret_redaction_covers_headers_urls_jwts_and_private_keys

echo "security verification passed"
