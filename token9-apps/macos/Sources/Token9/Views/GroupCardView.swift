import Charts
import SwiftUI

struct GroupCardView: View {
    let card: GroupCard
    let subTitle: String  // "按模型" or "按工具"
    @State private var expanded = false

    var body: some View {
        Card(tint: card.tint) {
            VStack(alignment: .leading, spacing: 12) {
                header
                Headline(value: Fmt.tokens(card.totalTokens), caption: "总量 tokens")
                metricGrid
                if !card.subs.isEmpty { subSection }
                if !rateRows.isEmpty {
                    Divider().overlay(Color.white.opacity(0.08))
                    ForEach(rateRows) { row in rateRow(row) }
                }
            }
        }
    }

    private var header: some View {
        HStack(spacing: 8) {
            Circle().fill(card.tint).frame(width: 9, height: 9)
                .shadow(color: card.tint.opacity(0.6), radius: 3)
            Text(card.name)
                .font(.system(size: 15, weight: .bold))
                .foregroundStyle(Theme.tPrimary)
                .lineLimit(1)
            Pill(text: "\(card.requests)", tint: card.tint)
            Spacer(minLength: 0)
        }
    }

    private var metricGrid: some View {
        LazyVGrid(
            columns: [GridItem(.flexible(), spacing: 10), GridItem(.flexible(), spacing: 10)],
            alignment: .leading, spacing: 10
        ) {
            MetricCell(icon: "arrow.down", label: "输入", value: Fmt.tokens(card.input), tint: card.tint)
            MetricCell(icon: "arrow.up", label: "输出", value: Fmt.tokens(card.output), tint: card.tint)
            MetricCell(icon: "bolt.fill", label: "缓存读", value: Fmt.tokens(card.cacheRead), tint: card.tint)
            MetricCell(icon: "square.stack.3d.up.fill", label: "缓存写", value: Fmt.tokens(card.cacheWrite), tint: card.tint)
            MetricCell(icon: "number", label: "请求", value: "\(card.requests)", tint: card.tint)
            RingMetricCell(value: card.cacheRatioPct, label: "Cache Hit", tint: card.tint)
        }
    }

    private var subSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            Button {
                withAnimation(.easeOut(duration: 0.18)) { expanded.toggle() }
            } label: {
                HStack(spacing: 5) {
                    Image(systemName: "chart.pie.fill").font(.system(size: 9))
                    Text("\(subTitle) (\(card.subs.count))").font(.system(size: 11, weight: .medium))
                    Image(systemName: expanded ? "chevron.down" : "chevron.right")
                        .font(.system(size: 8, weight: .bold))
                    Spacer(minLength: 0)
                }
                .foregroundStyle(card.tint)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)

            if expanded {
                VStack(spacing: 6) {
                    ForEach(card.subs) { s in subRow(s) }
                }
            }
        }
    }

    private func subRow(_ s: SubLine) -> some View {
        HStack(spacing: 7) {
            Circle().fill(card.tint.opacity(0.7)).frame(width: 5, height: 5)
            Text(s.name).font(.system(size: 11)).foregroundStyle(Theme.tSecondary).lineLimit(1)
            Spacer(minLength: 6)
            Text("\(Int(s.cacheRatio.rounded()))%")
                .font(.system(size: 9.5, design: .monospaced)).foregroundStyle(Theme.tTertiary)
            Text(Fmt.tokens(s.tokens))
                .font(.system(size: 10.5, weight: .semibold, design: .monospaced))
                .foregroundStyle(Theme.tPrimary)
        }
    }

    // ---- rate limits ----

    private struct RateRow: Identifiable {
        let id: String
        let label: String
        let remaining: Int
        let limit: Int
        var pct: Double { limit > 0 ? Double(remaining) / Double(limit) * 100 : 0 }
    }

    private var rateRows: [RateRow] {
        var rows: [RateRow] = []
        for rl in card.rateLimits {
            if let lim = rl.requests_limit, let rem = rl.requests_remaining, lim > 0 {
                rows.append(RateRow(id: "\(rl.provider)-req", label: "\(rl.provider) 请求剩余", remaining: Int(rem), limit: Int(lim)))
            }
            if let lim = rl.tokens_limit, let rem = rl.tokens_remaining, lim > 0 {
                rows.append(RateRow(id: "\(rl.provider)-tok", label: "\(rl.provider) tokens剩余", remaining: Int(rem), limit: Int(lim)))
            }
        }
        return rows
    }

    private func rateRow(_ row: RateRow) -> some View {
        let low = row.pct <= 15
        return VStack(alignment: .leading, spacing: 4) {
            HStack {
                Text(row.label).font(.system(size: 10)).foregroundStyle(Theme.tTertiary)
                Spacer()
                Text("\(Int(row.pct.rounded()))%")
                    .font(.system(size: 10, weight: .semibold, design: .monospaced))
                    .foregroundStyle(low ? Color.red : Theme.tSecondary)
            }
            MiniBar(value: row.pct, tint: low ? .red : card.tint)
        }
    }
}

/// Compact horizontal bar chart of token totals per group.
struct DistributionChart: View {
    let cards: [GroupCard]

    var body: some View {
        Chart(cards) { c in
            BarMark(
                x: .value("tokens", Int(c.totalTokens)),
                y: .value("name", c.name)
            )
            .foregroundStyle(c.tint.gradient)
            .cornerRadius(4)
            .annotation(position: .trailing, alignment: .leading) {
                Text(Fmt.tokens(c.totalTokens))
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(Theme.tTertiary)
            }
        }
        .chartXAxis(.hidden)
        .chartYAxis {
            AxisMarks(position: .leading) { _ in
                AxisValueLabel()
            }
        }
        .frame(height: CGFloat(max(1, cards.count)) * 26 + 8)
    }
}
