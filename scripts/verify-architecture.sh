#!/bin/sh
set -eu

repository=$(git rev-parse --show-toplevel)
cd "$repository"

cargo metadata --locked --no-deps --format-version 1 | python3 -c '
import json
import sys

metadata = json.load(sys.stdin)
workspace_ids = set(metadata["workspace_members"])
packages = {
    package["name"]: package
    for package in metadata["packages"]
    if package["id"] in workspace_ids
}

def normal_first_party(name):
    return {
        dependency["name"]
        for dependency in packages[name]["dependencies"]
        if dependency["kind"] is None and dependency["name"] in packages
    }

def normal_all(name):
    return {
        dependency["name"]
        for dependency in packages[name]["dependencies"]
        if dependency["kind"] is None
    }

errors = []

agent_dependencies = normal_first_party("sylvander-agent")
if agent_dependencies != {"sylvander-llm-core"}:
    errors.append(
        "sylvander-agent normal first-party dependencies must be exactly "
        f"sylvander-llm-core; found {sorted(agent_dependencies)}"
    )

protocol_banned = {
    "async-trait",
    "reqwest",
    "rusqlite",
    "sylvander-agent",
    "sylvander-runtime",
    "tokio",
}
protocol_dependencies = normal_all("sylvander-api")
found = sorted(protocol_dependencies & protocol_banned)
if found:
    errors.append(f"sylvander-api has forbidden runtime dependencies: {found}")

for name in sorted(packages):
    dependencies = normal_first_party(name)
    if name.startswith("sylvander-channel") and "sylvander-agent" in dependencies:
        errors.append(f"{name} must not depend on sylvander-agent")
    if name.startswith("sylvander-llm-") and name != "sylvander-llm-core":
        unexpected = dependencies - {"sylvander-llm-core"}
        if unexpected:
            errors.append(
                f"{name} has forbidden first-party dependencies: {sorted(unexpected)}"
            )
    if (
        name != "sylvander-runtime"
        and "sylvander-agent" in dependencies
        and "sylvander-api" in dependencies
    ):
        errors.append(f"{name} joins Agent and public API; only sylvander-runtime may do so")

if errors:
    for error in errors:
        print(f"architecture verification: {error}", file=sys.stderr)
    raise SystemExit(1)

print(
    "architecture verification: Agent, API, Channel, provider, and Runtime "
    "dependency boundaries passed"
)
'

if rg -n 'sylvander_api::types::|crate::types::' --glob '*.rs' .; then
  echo "architecture verification: deleted API types path is still referenced" >&2
  exit 1
fi

if rg -n '^[[:space:]]+use[[:space:]]' --glob '*.rs' \
  sylvander-api/src sylvander-channel/src sylvander-agent/src; then
  echo "architecture verification: nested Rust use declaration found in boundary crates" >&2
  exit 1
fi

echo "architecture source-boundary verification passed"
