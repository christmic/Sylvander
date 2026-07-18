#!/bin/sh
set -eu

repository="$(git rev-parse --show-toplevel)"
room="$(mktemp -d "${TMPDIR:-/tmp}/sylvander-clean-room.XXXXXX")"
server_pid=

cleanup() {
  if [ -n "$server_pid" ] && kill -0 "$server_pid" 2>/dev/null; then
    kill -INT "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
  fi
  rm -rf "$room"
}
trap cleanup EXIT HUP INT TERM

mkdir -p "$room/source" "$room/install" "$room/data" "$room/workspace" \
  "$room/integrity"
git -C "$repository" archive HEAD | tar -x -C "$room/source"

export CARGO_TARGET_DIR="$repository/target/clean-room"
export CARGO_HTTP_PROXY=
export CARGO_HTTPS_PROXY=
cargo install --path "$room/source/sylvander-server" \
  --root "$room/install" --locked --offline --force
cargo install --path "$room/source/sylvander-tui" \
  --root "$room/install" --locked --offline --force

config="$room/server.toml"
cat >"$config" <<EOF
schema_version = 1

[server]
name = "clean-room"
data_dir = "$room/data"

[server.memory_maintenance.integrity]

[server.memory_maintenance.integrity.key]
source = "env"
name = "SYLVANDER_MEMORY_INTEGRITY_KEY"

[server.memory_maintenance.integrity.backend]
kind = "file"
anchor_path = "$room/integrity/anchor.json"

[[model_providers]]
id = "fixture"
kind = "anthropic_compatible"
base_url = "http://127.0.0.1:9"

[model_providers.api_key]
source = "env"
name = "SYLVANDER_CLEAN_ROOM_MODEL_KEY"

[[model_providers.models]]
id = "fixture-model"
context_window = 32768
max_output_tokens = 4096
capabilities = ["tool_use"]

[[execution_targets]]
id = "local"

[execution_targets.transport]
kind = "local"
root = "$room"

[[agents]]
revision = 1
allow_session_prompt = false

[agents.access]
allow_authenticated = true

[agents.spec]
id = "sylvander"
name = "Sylvander"

[agents.spec.persona]
system_prompt = "Clean-room verification Agent."
description = "Release verification"

[agents.spec.model]
provider = "fixture"
model_name = "fixture-model"
allowed_models = [{ provider_id = "fixture", model_id = "fixture-model" }]
max_tokens = 4096

[agents.agent_workspace]
execution_target = "local"
path = "$room/workspace"
read_only = false

[[channels]]
id = "terminal"
enabled = true
default_agent = "sylvander"

[channels.transport]
kind = "unix"
path = "$room/sylvander.sock"
EOF

export SYLVANDER_CONFIG="$config"
export SYLVANDER_MEMORY_INTEGRITY_KEY="clean-room-integrity-key-32-bytes"
export SYLVANDER_CLEAN_ROOM_MODEL_KEY="clean-room-fixture-key"
"$room/install/bin/sylvander" >"$room/server.log" 2>&1 &
server_pid=$!

attempt=0
while [ "$attempt" -lt 100 ] && [ ! -S "$room/sylvander.sock" ]; do
  if ! kill -0 "$server_pid" 2>/dev/null; then
    cat "$room/server.log" >&2
    exit 1
  fi
  attempt=$((attempt + 1))
  sleep 0.05
done
[ -S "$room/sylvander.sock" ] || {
  cat "$room/server.log" >&2
  echo "clean-room server did not create its Unix socket" >&2
  exit 1
}

[ -x "$room/install/bin/sylvander-tui" ]
kill -0 "$server_pid"
[ -f "$room/data/sessions.db" ]
[ -f "$room/data/memory.db" ]
kill -INT "$server_pid"
if ! wait "$server_pid"; then
  server_pid=
  cat "$room/server.log" >&2
  echo "clean-room server did not shut down cleanly" >&2
  exit 1
fi
server_pid=

echo "clean-room install, startup, readiness, and shutdown passed"
