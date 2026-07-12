# token9 macOS Dashboard v2 — Implementation Checklist

Audience: implementation agent with limited product judgment  
Target: native macOS 14+ menu-bar application built with SwiftUI and Swift Package Manager  
Design source of truth: this directory's three PNG boards and `README.md`

## 0. Operating rules

Follow every rule below. Do not reinterpret the design.

- Work only under `sylvander-token9/` unless this checklist explicitly names a
  source brand asset outside that directory.
- Do not edit the Rust API contract or server for this UI task. The existing
  `/stats/summary` response already contains daily buckets.
- Do not hand-edit `Generated/Contracts.swift`.
- Do not add third-party Swift packages. Use SwiftUI, AppKit, and Foundation.
- Do not add rank badges. List order is the rank.
- Do not restore a large standalone tool/model segmented control.
- Do not crop a logo from a PNG mockup.
- Keep `MenuBarExtra` and `.menuBarExtraStyle(.window)`.
- Keep the dashboard dark-only for this milestone.
- Make one phase per commit. Do not combine unfinished phases.
- Run the verification command at the end of every phase. Stop on failure.

## 1. Verified baseline

Read these files before changing anything:

- `token9-apps/macos/Package.swift`
- `token9-apps/macos/Sources/Token9/Token9App.swift`
- `token9-apps/macos/Sources/Token9/DesignSystem.swift`
- `token9-apps/macos/Sources/Token9/RangeKey.swift`
- `token9-apps/macos/Sources/Token9/Format.swift`
- `token9-apps/macos/Sources/Token9/Api/Token9Client.swift`
- `token9-apps/macos/Sources/Token9/ViewModel/DashboardViewModel.swift`
- `token9-apps/macos/Sources/Token9/Views/DashboardView.swift`
- `token9-apps/macos/Sources/Token9/Views/GroupCardView.swift`
- `docs/design/token9-dashboard-v2/README.md`
- All three PNG boards beside this checklist.

Confirmed facts:

- The package targets macOS 14 and has one executable target named `Token9`.
- The popover currently has a fixed `440 x 620` frame.
- `RangeKey` supplies inclusive local-date query bounds.
- `StatBucketDto.date` is a `YYYY-MM-DD` string.
- A bucket contains provider, model, tool, request count, and four token counts.
- `DashboardViewModel` refreshes every 30 seconds.
- `DashboardViewModel.aggregate` already sorts groups by total tokens descending.
- Rate-limit data is optional and must not block usage data.

Baseline verification:

```bash
cd sylvander-token9/token9-apps/macos
swift build
```

Expected result: exit code 0 and a built `Token9` executable.

## 2. Required file layout

Create or modify only the following macOS source files for this milestone:

```text
Sources/Token9/
├── Api/Token9Client.swift                 # no functional change expected
├── DesignSystem.swift                     # replace visual tokens/components
├── Format.swift                           # add percentage/date helpers only
├── RangeKey.swift                         # add heatmap visibility/labels
├── Resources/
│   └── SeedCrabMark.pdf                   # approved vector asset
├── Token9App.swift                        # menu-bar icon/resource wiring
├── ViewModel/
│   └── DashboardViewModel.swift           # summary + daily aggregation + expansion
└── Views/
    ├── ActivityHeatmapView.swift           # new
    ├── DashboardView.swift                 # new hierarchy
    ├── GroupRowView.swift                  # replaces large cards
    └── SummaryStripView.swift              # new
Tests/Token9Tests/
├── DashboardAggregationTests.swift         # new
├── HeatmapScaleTests.swift                 # new
└── RangeKeyTests.swift                     # new
```

Delete `Views/GroupCardView.swift` only after `GroupRowView.swift` compiles and
all references have moved. Do not keep two competing implementations.

Update `Package.swift`:

- Add `.process("Resources")` to the executable target.
- Add a test target named `Token9Tests` depending on `Token9`.
- Do not change the macOS 14 deployment target.

## 3. Phase A — brand asset and design tokens

### A1. Prepare the mark

- Source brand references:
  - `../docs/design/final-brand/sylvander-logo-system.png`
  - `../docs/design/final-brand/sylvander-seed-crab-character-faithful.svg`
- Export the simplified two-tone seed-crab mark as a vector PDF named exactly
  `SeedCrabMark.pdf`.
- The mark must have a transparent background and no text.
- Warm/left portion: seed orange. Cool/right portion: core violet.
- Do not use the full 3D mascot in the popover.
- Add the PDF as an SPM resource and load it with
  `Image("SeedCrabMark", bundle: .module)`.

If an approved isolated vector cannot be exported, stop this phase and report
the missing asset. Do not approximate the logo with an SF Symbol.

### A2. Replace theme tokens

In `DesignSystem.swift`, define one centralized token set:

```text
backgroundPrimary  #111319
backgroundElevated white at 4-6% opacity
borderSubtle       white at 6-10% opacity
seedOrange         #F18A67
seedOrangeDeep     #C85A3D
coreViolet         #7657D6
electricBlue       #4387E5
healthyMint        #58D49B
warningAmber       system orange or equivalent
textPrimary        white at 96-100%
textSecondary      white at 70-76%
textTertiary       white at 48-58%
```

Use these layout constants:

- Popover width: `480` points.
- Popover height: `660` points.
- Outer padding: `16` points.
- Major vertical gap: `14` points.
- Card radius: `12` points.
- Row radius: `12` points.
- Hairline border: `0.75` points.

Remove hover scale effects. Hover may change fill or border opacity only.

### A3. Build reusable primitives

Implement these private or internal SwiftUI components in `DesignSystem.swift`:

- `StatusDot(active:)`
- `IconButton(systemName:action:)`
- `Panel(content:)`
- `MetricIcon(systemName:tint:)`
- `MiniBar(value:tint:)`
- `CacheRing(value:tint:)`
- `RangeTabs(selection:)`
- `DimensionToggle(selection:)`

`DimensionToggle` requirements:

- Text labels only: `工具` and `模型`.
- Selected label uses seed orange and a 2-point underline.
- No full-width background container.
- No large bordered segmented control.

Phase A verification:

```bash
cd sylvander-token9/token9-apps/macos
swift build
```

Visual acceptance:

- Header logo is crisp at 34 x 34 points on a Retina display.
- No component moves or scales on hover.
- Tool/model control is visually weaker than the time-range control.

## 4. Phase B — deterministic dashboard data model

### B1. Add derived models

Add these value types to `DashboardViewModel.swift` or a new file in the same
folder. Use the exact semantics below.

`DashboardSummary`:

- `totalTokens: Int64`
- `requests: Int64`
- `cacheReadTokens: Int64`
- `inputTokens: Int64`
- `cacheHitPercent: Double`

Formulas:

```text
totalTokens = input + output + cacheRead + cacheWrite
cacheHitPercent = cacheRead / (input + cacheRead) * 100
cacheHitPercent = 0 when denominator is 0
```

`DailyUsage`:

- `date: Date`
- `dateKey: String`
- `tokens: Int64`
- `requests: Int64`
- `hasData: Bool`

`GroupRow`:

- Reuse the existing `GroupCard` fields or rename it.
- Add `sharePercent: Double`, calculated against the sum of all group totals.
- Keep groups sorted descending by `totalTokens`.
- Do not add a rank field.

### B2. Aggregate daily usage

Aggregate all buckets by `StatBucketDto.date` regardless of `groupBy`.

- Sum all four token fields for each date.
- Sum requests for each date.
- Fill every calendar date in the selected range.
- A filled date with no bucket has `tokens = 0`, `requests = 0`, and
  `hasData = true` after a successful API response.
- Use `hasData = false` only when the whole request failed or a future API
  explicitly identifies missing data.
- Parse dates with a Gregorian calendar and local time zone.
- Never parse `YYYY-MM-DD` using an implicit locale-dependent formatter.

### B3. Expansion state

- Move expanded-row ownership to the parent dashboard.
- Store `expandedGroupID: String?` in `DashboardView` or the view model.
- Clicking a collapsed row sets its ID.
- Clicking the same expanded row sets `nil`.
- Clicking another row replaces the previous ID.
- At most one row is expanded.

### B4. Loading behavior

- Keep the existing 30-second refresh timer.
- Preserve existing cards and heatmap while refreshing.
- Show a small header `ProgressView`; do not replace content with a spinner.
- On refresh failure with existing data, retain data and show offline status.
- On initial failure with no data, show the connection error state.

Phase B tests:

- Summary totals across multiple buckets.
- Cache denominator zero.
- Daily aggregation combines provider/tool/model buckets on the same day.
- Missing calendar days are filled with zero.
- Tool/model changes do not change daily heatmap totals.
- Group shares sum to approximately 100% when data is non-empty.
- Group order is descending and stable for equal totals by name ascending.

Verification:

```bash
cd sylvander-token9/token9-apps/macos
swift test
swift build
```

## 5. Phase C — fixed header, range tabs, and summary strip

Rebuild `DashboardView.swift` in this exact vertical order:

1. Gateway header.
2. Time-range tabs.
3. Three summary cards.
4. Heatmap when eligible.
5. Aggregation heading and dimension toggle.
6. Scrollable group rows.
7. Rate-limit warning when applicable.

### C1. Header

- Show the 34 x 34 seed-crab mark at leading edge.
- Title: `token9`.
- Subtitle: `本地 LLM 网关`.
- Status line: `在线 · 127.0.0.1:9527` or `离线 · 127.0.0.1:9527`.
- Online uses healthy mint; offline uses warning/error color.
- Trailing actions: refresh and appearance only.
- Remove the continuously displayed clock.
- Keep the subtle orbital-node background in the header only. Implement it
  with lightweight SwiftUI shapes; it must not intercept pointer events.

### C2. Time range

- Preserve six cases in this order: yesterday, today, week, lastWeek, month,
  year.
- Preserve current Chinese labels.
- Use the selected seed-orange outline shown in the boards.
- Minimum hit target for each tab: 36 points high.

### C3. Summary strip

Implement `SummaryStripView` as three equal-width cards:

- `总流量`: formatted total tokens.
- `请求`: formatted request count.
- `缓存命中`: rounded whole percentage and cache ring.

Do not implement fake period-over-period percentages until comparison data is
available. Do not make a second API request for comparison in this milestone.
Do not show decorative sparklines.

Phase C visual acceptance:

- Match board 01 for overall density and hierarchy.
- Header and time range remain visible while the list scrolls.
- Summary cards remain one row at 480-point width.
- Text does not truncate at default macOS text size.

## 6. Phase D — activity heatmap

Create `Views/ActivityHeatmapView.swift`.

### D1. Visibility and geometry

Add to `RangeKey`:

- `showsHeatmap: Bool`
- `heatmapTitle: String`

Rules:

| Range | Visible | Geometry |
| --- | --- | --- |
| Yesterday | No | none |
| Today | No | none |
| This week | Yes | 7 daily cells |
| Last week | Yes | 7 daily cells |
| This month | Yes | calendar week columns x 7 rows |
| This year | Yes | approximately 53 columns x 7 rows |

For month and year:

- Column is calendar week.
- Row is weekday.
- Use the user's current calendar first weekday.
- Month view cells: 10-12 points, with 3-point gaps.
- Year view cells: calculate width from available geometry; minimum 4 points,
  maximum 8 points, with 2-point gaps.
- Never add horizontal scrolling.

### D2. Intensity scale

Create a pure function that returns levels 0 through 4.

- Level 0: zero tokens.
- Levels 1-4: quartiles of non-zero daily token values within the selected
  range.
- If all non-zero values are equal, assign them level 3.
- Colors: graphite, deep violet, core violet/electric blue, blue, seed orange.
- The scale must never divide by zero.

### D3. Labels and interaction

- Panel title: `每日用量`.
- Subtitle: current week range, `yyyy年M月`, or `yyyy`.
- Legend: `少` + five cells + `多`.
- Month/year: sparse weekday labels `一`, `三`, `五`.
- Year: month labels along the top; suppress overlapping labels.
- Hover/focus tooltip content:
  - localized date
  - formatted tokens
  - request count
- Tooltip example: `7月10日 · 1.2M tokens · 63 次请求`.
- Mouse hover and keyboard focus must reveal identical information.
- Add an accessibility label to every cell.

Phase D tests:

- Empty input produces only level 0.
- Equal values do not crash and use level 3.
- Quartile boundaries map deterministically.
- Week has 7 date entries.
- Leap-year annual range includes February 29.
- Month placement respects the calendar's first weekday.

Phase D visual acceptance:

- Match board 02 for month layout.
- Match board 03 for year density.
- Heatmap remains secondary to summary cards.
- Switching tool/model does not recolor or recalculate the heatmap.

## 7. Phase E — compact aggregation rows

Create `Views/GroupRowView.swift` and remove the old distribution chart.

Collapsed row order:

1. Name, left aligned.
2. Proportional traffic bar.
3. Formatted total tokens.
4. Rounded share percentage.
5. Disclosure chevron.

Requirements:

- No colored dot before the name.
- No rank number.
- No request-count pill beside the name.
- Bar width is `group.totalTokens / largestGroup.totalTokens`.
- Share text is `group.totalTokens / allGroupsTotal`.
- Zero total produces a zero-width fill without NaN.
- Entire row is clickable and has a minimum 44-point hit height.

Expanded content is a 2 x 3 metric grid in this order:

1. Input.
2. Output.
3. Cache read.
4. Cache write.
5. Requests.
6. Cache hit.

Below the grid, show the existing secondary-dimension disclosure such as
`按模型 (3)`. Preserve existing sub-lines when it expands. Do not silently drop
that functionality.

Remove `DistributionChart`; row bars now communicate distribution.

Phase E visual acceptance:

- Match board 01 for an expanded tool.
- Match board 02 for an expanded model.
- Match board 03 for all-collapsed rows.
- Only one row expands at a time.
- Sorting is visible through order, never through rank labels.

## 8. Phase F — states and rate-limit warning

Implement and verify these states:

### Loading with no data

- Keep header and range tabs visible.
- Center a small progress indicator in the content region.

### Empty successful response

- Title: `等待第一条流量`.
- Message: `将 AI 工具的 Base URL 指向 127.0.0.1:9527`.
- Do not show summary cards, heatmap, or empty group rows.

### Offline with no cached data

- Header status is offline.
- Title: `网关未连接`.
- Message: `启动 token9 serve 后，这里会自动恢复`.
- Refresh button remains enabled.

### Offline with cached data

- Keep last successful content visible.
- Header shows offline.
- Show the last successful update time in a tooltip or tertiary caption.

### Rate-limit warning

- Compute the minimum remaining percentage from available request/token limits.
- Show bottom warning only at 15% or lower.
- Copy: `接近速率限制 · 剩余 N%`.
- Do not show a settings action unless it opens a real implemented destination.
- Missing rate-limit headers are not an error.

## 9. Phase G — automated and visual verification

### G1. Required commands

Run from repository root:

```bash
cargo test --manifest-path sylvander-token9/Cargo.toml
cd sylvander-token9/token9-apps/macos
swift test
swift build -c release
```

Then build the app bundle from repository root:

```bash
bash sylvander-token9/scripts/build-macos.sh
```

Expected artifact:

```text
sylvander-token9/token9-apps/macos/Token9.app
```

### G2. Manual scenario matrix

Capture one screenshot for every row:

| ID | Range | Dimension | Data state | Expansion | Expected heatmap |
| --- | --- | --- | --- | --- | --- |
| V1 | Yesterday | Tool | populated | first row | hidden |
| V2 | Today | Model | populated | none | hidden |
| V3 | This week | Tool | populated | none | 7 days |
| V4 | Last week | Model | populated | first row | 7 days |
| V5 | This month | Model | populated | first row | month grid |
| V6 | This year | Tool | populated | none | 53-week grid |
| V7 | Today | Tool | empty | none | hidden |
| V8 | Month | Tool | initial offline | none | no content |
| V9 | Year | Model | cached offline | none | retained |
| V10 | Month | Tool | rate limit 12% | none | warning visible |

Compare V1, V5, and V6 against the three design boards at 100% zoom.

### G3. Interaction checks

- All six range tabs reload data exactly once.
- Tool/model toggle rebuilds rows without making a network request.
- Heatmap totals stay unchanged after tool/model toggle.
- Refresh does not blank existing content.
- Only one group row expands.
- Heatmap tooltip does not leave the popover bounds.
- Escape closes the menu-bar popover using native behavior.
- VoiceOver reads header status, summary metrics, heatmap cells, and rows.
- Reduce Motion removes non-essential matched-geometry or spring animation.

### G4. Final acceptance gate

Do not mark implementation complete until all statements are true:

- Debug build passes.
- Release build passes.
- Swift tests pass.
- Rust tests pass.
- App bundle exists and launches on macOS 14 or later.
- V1-V10 screenshots exist.
- V1/V5/V6 visually match their source boards in hierarchy and density.
- No numeric rank badges exist.
- Tool/model toggle is secondary to time range.
- Single-day views contain no heatmap.
- Month and year heatmaps fit without horizontal scrolling.
- No fabricated comparison percentages appear.
- No generated contract file was edited manually.
- No third-party dependency was added.

## 10. Commit sequence

Use this exact sequence:

1. `assets: add token9 seed crab mark`
2. `refactor(macos): establish dashboard v2 design system`
3. `feat(macos): add dashboard summary and daily aggregation`
4. `feat(macos): add adaptive usage heatmap`
5. `feat(macos): replace cards with compact aggregation rows`
6. `feat(macos): complete dashboard states and rate-limit warning`
7. `test(macos): cover dashboard ranges aggregation and heatmap scale`
8. `docs(macos): add dashboard v2 verification evidence`

Each commit must build independently. If a phase cannot build independently,
keep it uncommitted until it does; do not commit broken intermediate code.
