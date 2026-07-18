# Rust Test Layout

Rust production modules and test implementations are kept in separate directory
trees. This makes the production surface readable without forcing private
implementation details into the public API merely for testing.

## Directory contract

Each crate uses these locations:

```text
crate/
├── src/                  production implementation only
└── tests/
    ├── *.rs              black-box integration and journey tests
    ├── support/          helpers shared by integration tests
    └── unit/             white-box unit tests linked from `src`
```

Prefer a normal integration test in `tests/*.rs` whenever the behavior is
observable through the crate's public contract. A unit test that must inspect
private state lives in `tests/unit/<module>.rs`; its production module contains
only the test-only bridge:

```rust
#[cfg(test)]
#[path = "../tests/unit/example.rs"]
mod tests;
```

Nested production modules adjust the relative path but preserve the same
`tests/unit` destination. Files below `tests/unit` are not Cargo integration
test roots, so they compile exactly once as the owning module's unit tests.

## Rules

- Do not place test functions, fixtures, fake services, or large `mod tests`
  blocks in `src`.
- Do not make an implementation detail `pub` solely to move a test.
- Prefer public-contract integration tests over white-box unit tests.
- Put reusable test-only constructors in `tests/support`, not in production
  modules.
- A migration must preserve test names and assertions unless a documented
  behavior change is intentional.
- Run the affected crate test suite, all-target Clippy, and workspace format
  check before merging a migration.

## Audit command

Source-embedded test implementations are found with:

```sh
rg --pcre2 -n '^\s*#\[(?:tokio::)?test(?:\([^\]]*\))?\]' \
  --glob '*/src/**/*.rs' --glob 'src/**/*.rs'
```

Inline conditional test modules are found separately:

```sh
rg --pcre2 -n -U \
  '^\s*#\[cfg\(test\)\]\n(?:\s*#\[[^\n]+\]\n)*\s*mod\s+\w+\s*\{' \
  --glob '*/src/**/*.rs' --glob 'src/**/*.rs'
```

The migration is complete only when both commands return no matches.
`#[cfg(test)]` path bridges and small test-only accessors may remain in
production modules; the test functions, fixtures, and assertions themselves
must live below `tests/`.
