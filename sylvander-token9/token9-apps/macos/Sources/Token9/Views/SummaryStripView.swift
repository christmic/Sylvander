import SwiftUI

/// Three equal-width summary cards: 总流量 / 请求 / 缓存命中.
/// Per checklist §5 C3: no fabricated period-over-period comparisons,
/// no second API request, no decorative sparklines.
struct SummaryStripView: View {
    var summary: DashboardSummary
    @Environment(\.dashboardPalette) private var palette

    var body: some View {
        HStack(spacing: 10) {
            card(
                icon: "sum",
                tint: palette.accent,
                label: "总流量",
                value: Fmt.tokens(summary.totalTokens)
            )
            card(
                icon: "number",
                tint: palette.accent,
                label: "请求",
                value: "\(summary.requests)"
            )
            cacheCard
        }
    }

    private func card(icon: String, tint: Color, label: String, value: String) -> some View {
        Panel(radius: L.cardRadius) {
            VStack(alignment: .leading, spacing: 6) {
                HStack(spacing: 6) {
                    MetricIcon(systemName: icon, tint: tint)
                    Text(label)
                        .font(.system(size: 10))
                        .foregroundStyle(T.textTertiary)
                }
                Text(value)
                    .font(.system(size: 20, weight: .bold, design: .rounded))
                    .foregroundStyle(T.textPrimary)
                    .lineLimit(1)
                    .minimumScaleFactor(0.7)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private var cacheCard: some View {
        Panel(radius: L.cardRadius) {
            VStack(alignment: .leading, spacing: 6) {
                Text("缓存命中")
                    .font(.system(size: 10))
                    .foregroundStyle(T.textTertiary)
                    .lineLimit(1)
                HStack(spacing: 8) {
                    Text(Fmt.percent(summary.cacheHitPercent))
                        .font(.system(size: 20, weight: .bold, design: .rounded))
                        .foregroundStyle(T.textPrimary)
                        .lineLimit(1)
                        .minimumScaleFactor(0.7)
                    Spacer(minLength: 0)
                    CacheRing(value: summary.cacheHitPercent, tint: palette.accent, lineWidth: 4)
                        .frame(width: 30, height: 30)
                }
            }
        }
    }
}
