#!/usr/bin/env bash
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

failures=0

report_matches() {
  local title="$1"
  shift
  local matches
  matches="$(git grep -nE "$@" -- ':(glob)*/src/**/*.rs' 2>/dev/null || true)"
  if [[ -n "$matches" ]]; then
    echo "$title" >&2
    echo "$matches" >&2
    failures=1
  fi
}

report_matches \
  "Rust test functions must live under the owning crate's tests/ tree:" \
  '#\[(tokio::|async_std::)?test([[:space:]]|\])|#\[rstest([[:space:]]|\])|#\[test_case'

report_matches \
  "Inline Rust test modules are forbidden; use #[path = \"../tests/...\"]:" \
  'mod[[:space:]]+(tests|test)[[:space:]]*\{'

source_test_files=""
while IFS= read -r file; do
  if [[ -f "$file" ]]; then
    source_test_files+="${source_test_files:+$'\n'}$file"
  fi
done < <(
  git ls-files \
    ':(glob)*/src/**/*_test.rs' \
    ':(glob)*/src/**/*_tests.rs' \
    ':(glob)*/src/**/tests.rs'
)
if [[ -n "$source_test_files" ]]; then
  echo "Rust test files must not be stored below src/:" >&2
  echo "$source_test_files" >&2
  failures=1
fi

if (( failures != 0 )); then
  exit 1
fi

echo "Rust test layout verified: test bodies and test files are outside src/."
