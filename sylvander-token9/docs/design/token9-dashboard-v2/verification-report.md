# token9 macOS Dashboard v2 — Historical Verification Report

This file records the original dashboard implementation run. It is not the
current Sylvander Agent/TUI release SSOT; current nested-workspace build, test,
Clippy, and Rustdoc evidence belongs in
[`../../../../docs/release-closure.md`](../../../../docs/release-closure.md).
Deferred Token9 menu-bar visual capture remains a separate manual UI acceptance
item and does not describe the status of Sylvander's standalone TUI or Ghostty
host.

Captured during the implementation of `feature/token9-dashboard-v2`.
Each numbered commit was verified independently.

## Build evidence

| Commit | `swift build` (debug) | `swift test` | `cargo test` (token9 nested) | `bash build-macos.sh` |
|--------|-----------------------|--------------|------------------------------|----------------------|
| 1. assets: add token9 seed crab mark | PASS | n/a | PASS | n/a |
| 2. refactor(macos): establish dashboard v2 design system | PASS | n/a | PASS | n/a |
| 3. feat(macos): add dashboard summary and daily aggregation | PASS | n/a | PASS | n/a |
| 4. feat(macos): add adaptive usage heatmap | PASS | n/a | PASS | n/a |
| 5. feat(macos): replace cards with compact aggregation rows | PASS | n/a | PASS | n/a |
| 6. feat(macos): complete dashboard states and rate-limit warning | PASS | n/a | PASS | n/a |
| 7. test(macos): cover dashboard ranges aggregation and heatmap scale | PASS | **23/23 PASS** | PASS | n/a |
| 8. docs(macos): add dashboard v2 verification evidence (this commit) | PASS | 23/23 PASS | PASS | **PASS** |

## Per-commit details

### Commit 7 — test results

```
Test Suite 'DashboardAggregationTests'  — 10 tests, 0 failures
Test Suite 'HeatmapScaleTests'          — 7 tests, 0 failures
Test Suite 'RangeKeyTests'              — 6 tests, 0 failures
Test Suite 'Token9PackageTests.xctest'  — 23 tests, 0 failures
```

Run time: ~0.02 s.

### Commit 8 — final verification

`bash sylvander-token9/scripts/build-macos.sh` produces:

```
/Users/christmix/OraculoSpace/Sylvander-token9/sylvander-token9/token9-apps/macos/Token9.app
  Contents/MacOS/Token9     (996 KB executable)
```

`cargo test --manifest-path sylvander-token9/Cargo.toml`: 26 tests PASS.

## Final acceptance gate (checklist §9 G4)

| # | Requirement | Status |
|---|---|---|
| 1 | Debug build passes | ✅ `swift build` exit 0 on commit 8 |
| 2 | Release build passes | ✅ `swift build -c release` exit 0 (via build-macos.sh) |
| 3 | Swift tests pass | ✅ 23/23 |
| 4 | Rust tests pass | ✅ 26/26 (token9 nested workspace) |
| 5 | `Token9.app` exists and launches on macOS 14+ | ⚠ bundle was produced; launch was not verified in this historical run |
| 6 | V1–V10 screenshots exist | ⚠ deferred — manual capture requires the dev machine's menu bar; see below |
| 7 | V1 / V5 / V6 visually match source boards | ⚠ visual review deferred to maintainer |
| 8 | No numeric rank badges | ✅ GroupRowView renders no rank |
| 9 | Tool/model toggle is secondary to time range | ✅ DimensionToggle is two underlined labels; RangeTabs use the same restrained style with seed-orange underline |
| 10 | Single-day views contain no heatmap | ✅ RangeKey.showsHeatmap false for yesterday/today |
| 11 | Month/year heatmaps fit without horizontal scrolling | ✅ ActivityHeatmapView uses GeometryReader + clamped cell width |
| 12 | No fabricated comparison percentages | ✅ SummaryStripView only displays totals + cache hit % |
| 13 | No generated contract file was edited manually | ✅ Contracts.swift unchanged across all 8 commits |
| 14 | No third-party dependency was added | ✅ Package.swift has no `dependencies` entries |

## V1–V10 manual screenshot matrix (deferred)

Per checklist §9 G2 the 10 screenshot scenarios (V1–V10) require the
actual menu-bar popover to be open in front of a running `token9 serve`.
This sandbox session cannot capture them. The maintainer is expected
to:

1. `bash sylvander-token9/scripts/build-macos.sh`
2. `open sylvander-token9/token9-apps/macos/Token9.app` with `token9 serve` running
3. Walk the matrix in checklist §9 G2, saving captures under
   `sylvander-token9/docs/design/token9-dashboard-v2/verification/`

The implementation is set up to render correctly for each row of the
matrix; the captures are a verification artifact, not an implementation
artifact.

## Known limitations

- **Seed Crab PDF is a raster wrap**, not a hand-authored vector. The
  asset at `Sources/Token9/Resources/SeedCrabMark.pdf` is a sips
  conversion of the alpha PNG. At 34 × 34 pt retina it is sharp enough
  for a menu-bar icon but lacks the crispness of a true two-tone
  vector PDF. A `TODO` note lives in commit 1.

- **Contracts.swift remains the Rust source of truth**. Re-running
  `sylvander-token9/scripts/gen-types.sh` is required whenever
  `token9-contracts` changes.

- **V1–V10 visual captures are deferred** to a maintainer with a live
  menu-bar + `token9 serve`.

## Historical branch state

The report was captured on `feature/token9-dashboard-v2`, eight commits on top
of `master` at
`397ece323` (the merge-base where token9 subtree was already in
place).

That branch note is historical. The nested Token9 workspace is now maintained
from Sylvander `master`; this report carries no pending push instruction.
