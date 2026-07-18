#!/usr/bin/env bash
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
fixture_root="$(mktemp -d "${TMPDIR:-/tmp}/sylvander-test-layout.XXXXXX")"
trap 'rm -rf "$fixture_root"' EXIT

git -C "$fixture_root" init --quiet
mkdir -p \
  "$fixture_root/scripts" \
  "$fixture_root/sylvander-token9/token9-server/src"
cp "$repo_root/scripts/verify-rust-test-layout.sh" "$fixture_root/scripts/"
chmod +x "$fixture_root/scripts/verify-rust-test-layout.sh"

printf '%s\n' \
  '#[test]' \
  'fn nested_test_body_must_be_rejected() {}' \
  >"$fixture_root/sylvander-token9/token9-server/src/nested.rs"
git -C "$fixture_root" add .

if output="$(cd "$fixture_root" && ./scripts/verify-rust-test-layout.sh 2>&1)"; then
  echo "expected nested test body to fail layout verification" >&2
  exit 1
fi

expected='sylvander-token9/token9-server/src/nested.rs:1:#[test]'
if [[ "$output" != *"$expected"* ]]; then
  echo "layout verifier did not report the nested token9 fixture" >&2
  echo "$output" >&2
  exit 1
fi

printf '%s\n' \
  'pub fn production_body() {}' \
  >"$fixture_root/sylvander-token9/token9-server/src/nested.rs"
git -C "$fixture_root" add .
(cd "$fixture_root" && ./scripts/verify-rust-test-layout.sh >/dev/null)

echo "Rust test layout regression verified: nested crate source is inspected."
