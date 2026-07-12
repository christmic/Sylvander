import SwiftUI

/// One tool/model row in the aggregation list.
///
/// Per checklist §7:
///   Collapsed: [name] [proportional bar] [total tokens] [share %] [chevron]
///   Every row remains expanded. The parent list owns vertical scrolling.
///
/// No colored dot, no rank number, no request pill beside the name.
/// Bar width = group.totalTokens / largestGroup.totalTokens.
/// Share text = group.totalTokens / sum(all.totalTokens).
/// Zero total -> zero-width fill, no NaN.
struct GroupRowView: View {
    var card: GroupCard
    var allTotal: Int64
    var largestTotal: Int64
    @Environment(\.dashboardPalette) private var palette
    private var tint: Color { palette.groupColor(card.name) }

    var body: some View {
        VStack(spacing: 0) {
            collapsedRow
            Divider().background(T.borderSubtle)
            expandedContent
                .padding(.top, 10)
                .padding(.bottom, 12)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .frame(minHeight: L.rowMinHit, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: L.rowRadius, style: .continuous)
                .fill(Color.white.opacity(0.04))
        )
        .overlay(
            RoundedRectangle(cornerRadius: L.rowRadius, style: .continuous)
                .strokeBorder(LinearGradient(
                    colors: [tint.opacity(0.72), palette.secondary.opacity(0.32)],
                    startPoint: .topLeading,
                    endPoint: .bottomTrailing
                ), lineWidth: L.hairline)
        )
    }

    // MARK: Collapsed

    private var collapsedRow: some View {
        HStack(spacing: 10) {
            Text(card.name)
                .font(.system(size: 13, weight: .medium))
                .foregroundStyle(T.textPrimary)
                .lineLimit(1)
                .truncationMode(.tail)
                .frame(width: 160, alignment: .leading)

            bar
                .frame(maxWidth: .infinity)

            Text(Fmt.tokens(card.totalTokens))
                .font(.system(size: 12, weight: .semibold, design: .monospaced))
                .foregroundStyle(T.textPrimary)
                .lineLimit(1)

            Text(Fmt.percent(card.sharePercent(allTotal: allTotal)))
                .font(.system(size: 11, design: .monospaced))
                .foregroundStyle(T.textTertiary)
                .frame(width: 38, alignment: .trailing)
                .lineLimit(1)

        }
    }

    private var bar: some View {
        GeometryReader { geo in
            let fraction: Double = {
                guard largestTotal > 0 else { return 0 }
                return Double(card.totalTokens) / Double(largestTotal)
            }()
            ZStack(alignment: .leading) {
                Capsule().fill(Color.white.opacity(0.07))
                Capsule()
                    .fill(LinearGradient(
                        colors: [tint, palette.secondary],
                        startPoint: .leading,
                        endPoint: .trailing
                    ))
                    .frame(width: max(0, geo.size.width * fraction))
            }
        }
        .frame(height: 5)
    }

    // MARK: Expanded

    private var expandedContent: some View {
        VStack(alignment: .leading, spacing: 10) {
            metricGrid
            if !card.rateLimits.isEmpty {
                rateLimitBars
            }
        }
    }

    private var metricGrid: some View {
        let cols = [GridItem(.flexible(), spacing: 10), GridItem(.flexible(), spacing: 10), GridItem(.flexible(), spacing: 10)]
        return LazyVGrid(columns: cols, alignment: .leading, spacing: 10) {
            metric("arrow.down", "输入", Fmt.tokens(card.input), palette.dataColor(0))
            metric("arrow.up", "输出", Fmt.tokens(card.output), palette.dataColor(1))
            metric("bolt.fill", "缓存读", Fmt.tokens(card.cacheRead), palette.dataColor(2))
            metric("tray.fill", "缓存写", Fmt.tokens(card.cacheWrite), palette.dataColor(3))
            metric("number", "请求", "\(card.requests)", palette.dataColor(1))
            metric("percent", "命中", Fmt.percent(card.cacheRatio * 100), palette.dataColor(0))
        }
    }

    private func metric(_ icon: String, _ label: String, _ value: String, _ tint: Color) -> some View {
        HStack(spacing: 6) {
            MetricIcon(systemName: icon, tint: tint)
            Text(value)
                .font(.system(size: 12, weight: .semibold, design: .monospaced))
                .foregroundStyle(T.textPrimary)
                .lineLimit(1)
            Spacer(minLength: 0)
        }
        .help(label)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(label)
        .accessibilityValue(value)
    }

    private var rateLimitBars: some View {
        VStack(alignment: .leading, spacing: 4) {
            ForEach(card.rateLimits, id: \.provider) { rl in
                rateLimitRow(rl)
            }
        }
    }

    private func rateLimitRow(_ rl: RateLimitDto) -> some View {
        let reqPct = ratePercent(remaining: rl.requests_remaining, limit: rl.requests_limit)
        let tokPct = ratePercent(remaining: rl.tokens_remaining,   limit: rl.tokens_limit)
        let minPct = min(reqPct ?? 100, tokPct ?? 100)
        let tint: Color = minPct <= 15 ? T.warningAmber : T.healthyMint
        return HStack(spacing: 8) {
            Text(rl.provider)
                .font(.system(size: 10))
                .foregroundStyle(T.textTertiary)
                .frame(width: 64, alignment: .leading)
            MiniBar(value: minPct, tint: tint)
            Text(Fmt.percent(minPct))
                .font(.system(size: 10, design: .monospaced))
                .foregroundStyle(T.textSecondary)
                .frame(width: 38, alignment: .trailing)
        }
    }

    private func ratePercent(remaining: Int32?, limit: Int32?) -> Double? {
        guard let r = remaining, let l = limit, l > 0 else { return nil }
        return Double(r) / Double(l) * 100
    }
}
