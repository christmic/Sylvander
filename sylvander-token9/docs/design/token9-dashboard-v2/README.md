# token9 Dashboard UI v2

Status: design proposal
Scope: macOS menu-bar popover only; no implementation changes

Implementation handoff: [IMPLEMENTATION_CHECKLIST.md](IMPLEMENTATION_CHECKLIST.md)

## Product direction

The dashboard is a compact observability surface for a local LLM gateway. It
prioritizes gateway status, total traffic, cache efficiency, and traffic
distribution. The Seed Crab identity appears as a simplified app mark and as a
restrained seed-orange to core-violet color system; the mascot itself does not
occupy the data area.

## Scenario boards

| Scenario | Board | Purpose |
| --- | --- | --- |
| Yesterday / tools | [01-yesterday-tools.png](01-yesterday-tools.png) | Single-day overview with one expanded tool row. The heatmap is intentionally hidden. |
| Month / models | [02-month-models-heatmap.png](02-month-models-heatmap.png) | Monthly daily-token heatmap, model aggregation, and hover detail. |
| Year / tools | [03-year-tools-heatmap.png](03-year-tools-heatmap.png) | Dense 53-week heatmap and collapsed tool rows for a long-range overview. |

## Information hierarchy

1. Gateway identity, connection state, endpoint, refresh, and appearance.
2. Time range.
3. Total traffic, request count, and cache-hit rate.
4. Daily token activity when the selected range supports comparison.
5. Tool/model aggregation and expandable row details.
6. Rate-limit warning and settings entry.

The tool/model control is a secondary text toggle aligned with the aggregation
heading. It must not compete with the time range. Rows are ordered by token
volume; explicit rank numbers are not shown.

## Daily activity heatmap

### Metric

- Each cell represents total tokens for one local calendar day.
- Total tokens = input + output + cache read + cache write, matching the current
  dashboard total.
- The heatmap remains gateway-wide when switching between tool and model. The
  aggregation toggle only changes the ranked list below it.
- A future filter drilled into one tool/model may scope the heatmap, but that
  state must be made explicit in the heatmap title.

### Range behavior

| Selected range | Heatmap behavior |
| --- | --- |
| Yesterday / Today | Hidden. A single cell is not a useful heatmap. |
| This week / Last week | Seven daily cells with weekday labels. |
| This month | Calendar-aligned week columns by seven weekday rows. |
| This year | Approximately 53 week columns by seven weekday rows with month labels. |

### Intensity and states

- Five levels: empty, low, medium, high, peak.
- Empty uses the neutral graphite surface.
- Activity progresses core violet -> electric blue -> seed orange.
- Thresholds should use quantiles inside the selected range, preventing one
  extreme day from flattening every other cell.
- Missing data and a true zero must remain distinguishable: missing uses a
  subtle diagonal texture or reduced outline; zero is a solid empty cell.
- Hover tooltip: date, total tokens, and request count.
- Keyboard focus exposes the same tooltip and uses a two-pixel focus ring.

## Aggregation rows

- Sorted descending by total tokens; the visual order is the rank.
- No numeric rank badges.
- Collapsed row: name, proportional bar, total tokens, share, disclosure.
- Expanded row: input, output, cache read, cache write, requests, and cache hit.
- Only one row expands at a time in the compact popover.

## Layout notes

- Target content width: 440-480 pt.
- The header and range selector remain fixed; content below can scroll.
- Monthly and yearly heatmaps should fit without horizontal scrolling.
- Summary cards may show period-over-period text, but should not add sparklines
  when the heatmap is present.
- Seed-orange indicates selection or peak activity; it is not a generic border
  color for every container.
