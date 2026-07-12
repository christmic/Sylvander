import SwiftUI

/// Pure scale function. Returns an integer in 0...4 mapping the given
/// tokens against the population of non-zero daily values. Per checklist
/// §6 D2: quartile boundaries deterministic, never divide by zero,
/// equal values produce level 3 (no crash).
enum HeatmapScale {
    static func level(tokens: Int64, allNonZero: [Int64]) -> Int {
        guard tokens > 0 else { return 0 }
        let sorted = allNonZero.filter { $0 > 0 }.sorted()
        guard !sorted.isEmpty else { return 3 }    // shouldn't happen if tokens > 0
        if Set(sorted).count == 1 { return 3 }
        let q = quartiles(sorted)
        if tokens <= q[0] { return 1 }
        if tokens <= q[1] { return 2 }
        if tokens <= q[2] { return 3 }
        return 4
    }

    /// Linear-interpolation quartile boundaries over the sorted
    /// non-zero values. q[0]=25%, q[1]=50%, q[2]=75%.
    static func quartiles(_ sortedAsc: [Int64]) -> [Int64] {
        precondition(!sortedAsc.isEmpty)
        func pick(_ p: Double) -> Int64 {
            let idx = Int(p * Double(sortedAsc.count - 1).rounded())
            return sortedAsc[min(max(0, idx), sortedAsc.count - 1)]
        }
        return [pick(0.25), pick(0.50), pick(0.75)]
    }
}

/// Activity heatmap panel. Supports three geometries per checklist
/// §6 D1:
///   - week / lastWeek: 7 columns × 1 row of daily cells
///   - month: calendar-week columns × 7 weekday rows
///   - year:  ~53 columns × 7 rows, cell width auto-fitted to the
///            available geometry, clamped to [4, 8] pt with 2 pt gaps
struct ActivityHeatmapView: View {
    var range: RangeKey
    var daily: [DailyUsage]
    var now: Date = Date()
    @Environment(\.dashboardPalette) private var palette

    var body: some View {
        Panel {
            VStack(alignment: .leading, spacing: 10) {
                header
                content
            }
        }
    }

    // MARK: Header

    private var header: some View {
        HStack(alignment: .center) {
            VStack(alignment: .leading, spacing: 2) {
                Text("每日用量")
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(T.textPrimary)
                Text(range.heatmapTitle(now: now))
                    .font(.system(size: 9.5, design: .monospaced))
                    .foregroundStyle(T.textTertiary)
            }
            Spacer()
            legend
        }
    }

    // MARK: Geometry dispatch

    @ViewBuilder
    private var content: some View {
        switch range {
        case .week, .lastWeek:
            weekStrip
        case .month:
            monthGrid
        case .year:
            yearGrid
        case .yesterday, .today:
            EmptyView()
        }
    }

    private var weekStrip: some View {
        HStack(spacing: 4) {
            ForEach(Array(weekSlots.enumerated()), id: \.offset) { index, day in
                VStack(spacing: 4) {
                    Text(weekdayLabel(for: weekSlotDates[index]))
                        .font(.system(size: 8))
                        .foregroundStyle(T.textTertiary)
                    if let day {
                        cell(level: level(for: day), size: 16)
                            .help(tooltip(day))
                            .accessibilityLabel(accessibilityLabel(day))
                    } else {
                        placeholder(size: 16)
                    }
                }
                .frame(maxWidth: .infinity)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var monthGrid: some View {
        // Lay the month out as weekday rows (Mon..Sun) × week columns
        // starting from the calendar's first weekday.
        let cells = monthCells
        let firstWeekday = Calendar.current.firstWeekday  // 1=Sun, 2=Mon...
        let cellSize: CGFloat = 11
        let gap: CGFloat = 3
        let totalCells = cells.count
        let weeks = Int((Double(totalCells) / 7.0).rounded(.up))
        return HStack(alignment: .top, spacing: 4) {
            VStack(alignment: .trailing, spacing: gap) {
                ForEach(weekdayLabels(first: firstWeekday), id: \.self) { l in
                    Text(l)
                        .font(.system(size: 8))
                        .foregroundStyle(T.textTertiary)
                        .frame(width: 12, height: cellSize)
                }
            }
            VStack(alignment: .leading, spacing: gap) {
                ForEach(0..<7, id: \.self) { dayIdx in
                    HStack(spacing: gap) {
                        ForEach(0..<weeks, id: \.self) { weekIdx in
                            let idx = weekIdx * 7 + dayIdx
                            if idx < cells.count, let d = cells[idx] {
                                cell(level: level(for: d), size: cellSize)
                                    .help(tooltip(d))
                                    .accessibilityLabel(accessibilityLabel(d))
                            } else {
                                Rectangle()
                                    .fill(Color.clear)
                                    .frame(width: cellSize, height: cellSize)
                            }
                        }
                    }
                }
            }
        }
    }

    private var yearGrid: some View {
        let weeks = yearCells
        let gap: CGFloat = 2
        return VStack(alignment: .leading, spacing: 5) {
            HStack(spacing: 0) {
                ForEach(1...12, id: \.self) { month in
                    Text("\(month)月")
                        .font(.system(size: 7.5))
                        .foregroundStyle(T.textTertiary)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
            HStack(alignment: .top, spacing: 4) {
                VStack(alignment: .trailing, spacing: gap) {
                    ForEach(Array(["一", "", "三", "", "五", "", ""].enumerated()), id: \.offset) { _, label in
                        Text(label).font(.system(size: 7.5)).foregroundStyle(T.textTertiary)
                            .frame(width: 10, height: cellSizeDefault)
                    }
                }
                GeometryReader { geo in
                    let avail = geo.size.width
                    let rawW = (avail - CGFloat(weeks.count - 1) * gap) / CGFloat(max(1, weeks.count))
                    let cellSize = min(8, max(4, rawW))
                    VStack(alignment: .leading, spacing: gap) {
                        ForEach(0..<7, id: \.self) { dayIdx in
                            HStack(spacing: gap) {
                                ForEach(0..<weeks.count, id: \.self) { wk in
                                    let d: DailyUsage? = (dayIdx < weeks[wk].count) ? weeks[wk][dayIdx] : nil
                                    cellOrPlaceholder(d, size: cellSize, showPlaceholder: true)
                                }
                            }
                        }
                    }
                }
            }
        }
        .frame(height: 7 * (cellSizeDefault + gap) + 16)
    }

    private let cellSizeDefault: CGFloat = 6

    // MARK: Legend

    private var legend: some View {
        HStack(spacing: 6) {
            Text("少").font(.system(size: 9)).foregroundStyle(T.textTertiary)
            ForEach(0..<5) { i in
                cell(level: i, size: 10)
            }
            Text("多").font(.system(size: 9)).foregroundStyle(T.textTertiary)
        }
    }

    // MARK: Cell

    @ViewBuilder
    private func cellOrPlaceholder(_ d: DailyUsage?, size: CGFloat, showPlaceholder: Bool = false) -> some View {
        if let d {
            cell(level: level(for: d), size: size)
                .help(tooltip(d))
                .accessibilityLabel(accessibilityLabel(d))
        } else {
            if showPlaceholder { placeholder(size: size) }
            else { Color.clear.frame(width: size, height: size) }
        }
    }

    private func placeholder(size: CGFloat) -> some View {
        RoundedRectangle(cornerRadius: 2, style: .continuous)
            .fill(Color.white.opacity(0.035))
            .overlay(
                RoundedRectangle(cornerRadius: 2, style: .continuous)
                    .stroke(Color.white.opacity(0.045), lineWidth: 0.5)
            )
            .frame(width: size, height: size)
    }

    private func cell(level: Int, size: CGFloat) -> some View {
        Rectangle()
            .fill(color(for: level))
            .frame(width: size, height: size)
            .overlay(
                RoundedRectangle(cornerRadius: 2, style: .continuous)
                    .stroke(Color.white.opacity(0.04), lineWidth: 0.5)
            )
    }

    private func color(for level: Int) -> Color {
        palette.heatmapLevels[max(0, min(palette.heatmapLevels.count - 1, level))]
    }

    // MARK: Data shaping

    /// Days in the order they should render — preserves API order so
    /// the heatmap and the underlying DailyUsage array stay aligned.
    private var orderedDays: [DailyUsage] { daily }

    private var weekSlotDates: [Date] {
        let from = range.range(now: now).from
        guard let start = Fmt.parseDateKey(from) else { return orderedDays.map(\.date) }
        return (0..<7).compactMap { Calendar.current.date(byAdding: .day, value: $0, to: start) }
    }

    private var weekSlots: [DailyUsage?] {
        let byKey = Dictionary(uniqueKeysWithValues: orderedDays.map { ($0.dateKey, $0) })
        return weekSlotDates.map { byKey[Fmt.dateKey($0)] }
    }

    /// Month cells ordered as a flat array of size N×7, pre-padded so
    /// the first day of the month lands on the calendar's first
    /// weekday. Filler entries are nil and rendered as transparent.
    private var monthCells: [DailyUsage?] {
        guard let first = daily.first else { return [] }
        let cal = Calendar.current
        let firstWeekday = cal.firstWeekday
        let weekdayOfFirst = ((cal.component(.weekday, from: first.date) - firstWeekday) + 7) % 7
        var out: [DailyUsage?] = Array(repeating: nil, count: weekdayOfFirst)
        out.append(contentsOf: daily.map { Optional($0) })
        // Pad the tail so the array length is a multiple of 7.
        let rem = out.count % 7
        if rem != 0 { out.append(contentsOf: Array(repeating: nil, count: 7 - rem)) }
        return out
    }

    /// Year cells as an array of weeks, each week is 7 entries (some nil).
    private var yearCells: [[DailyUsage?]] {
        let cal = Calendar.current
        // Anchor to Jan 1 of the year so we get a fixed column count.
        let comps = cal.dateComponents([.year], from: now)
        guard let yearStart = cal.date(from: comps),
              let yearEnd = cal.date(byAdding: .year, value: 1, to: yearStart)
        else { return [] }
        let firstWeekday = cal.firstWeekday
        let pad = ((cal.component(.weekday, from: yearStart) - firstWeekday) + 7) % 7
        let byKey = Dictionary(uniqueKeysWithValues: orderedDays.map { ($0.dateKey, $0) })
        var out: [[DailyUsage?]] = []
        var current: [DailyUsage?] = Array(repeating: nil, count: pad)
        var cursor = yearStart
        while cursor < yearEnd {
            current.append(byKey[Fmt.dateKey(cursor, calendar: cal)])
            guard let next = cal.date(byAdding: .day, value: 1, to: cursor) else { break }
            cursor = next
        }
        // Pad tail to multiple of 7.
        let rem = current.count % 7
        if rem != 0 { current.append(contentsOf: Array(repeating: nil, count: 7 - rem)) }
        // Chunk into weeks.
        var i = 0
        while i < current.count {
            out.append(Array(current[i..<min(i + 7, current.count)]))
            i += 7
        }
        _ = yearEnd  // (placeholder if we later want last-week clipping)
        return out
    }

    // MARK: Lookup helpers

    private func level(for day: DailyUsage?) -> Int {
        guard let day else { return 0 }
        let nonZero = daily.map { $0.tokens }
        return HeatmapScale.level(tokens: day.tokens, allNonZero: nonZero)
    }

    private func tooltip(_ day: DailyUsage) -> String {
        let date = Fmt.shortDate(day.date)
        let tokens = Fmt.tokens(day.tokens)
        return "\(date) · \(tokens) tokens · \(day.requests) 次请求"
    }

    private func accessibilityLabel(_ day: DailyUsage) -> String {
        let date = Fmt.dateKey(day.date)
        return "\(date), \(Fmt.tokens(day.tokens)) tokens, \(day.requests) requests"
    }

    private func weekdayLabels(first: Int) -> [String] {
        // Produce the 7 weekday labels in calendar order, starting at
        // the calendar's first weekday. Show only 1/3/5 per checklist.
        let symbols = ["日", "一", "二", "三", "四", "五", "六"]
        let labels = Array(symbols[(first - 1)..<symbols.count] + symbols[0..<(first - 1)])
        return labels.enumerated().map { i, label in
            (i == 0 || i == 2 || i == 4) ? label : ""
        }
    }

    private func weekdayLabel(for date: Date) -> String {
        let symbols = ["日", "一", "二", "三", "四", "五", "六"]
        return symbols[Calendar.current.component(.weekday, from: date) - 1]
    }
}
