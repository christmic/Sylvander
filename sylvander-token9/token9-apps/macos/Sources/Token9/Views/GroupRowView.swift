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
    var subTitle: String
    private var tint: Color { T.groupTint(card.name) }

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
                .strokeBorder(tint.opacity(0.55), lineWidth: L.hairline)
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
                    .fill(tint)
                    .frame(width: max(0, geo.size.width * fraction))
            }
        }
        .frame(height: 5)
    }

    // MARK: Expanded

    private var expandedContent: some View {
        VStack(alignment: .leading, spacing: 10) {
            metricGrid
            if !card.subs.isEmpty {
                subDisclosure
            }
            if !card.rateLimits.isEmpty {
                rateLimitBars
            }
        }
    }

    private var metricGrid: some View {
        let cols = [GridItem(.flexible(), spacing: 10), GridItem(.flexible(), spacing: 10), GridItem(.flexible(), spacing: 10)]
        return LazyVGrid(columns: cols, alignment: .leading, spacing: 10) {
            metric("arrow.down", "输入",    Fmt.tokens(card.input),       T.electricBlue)
            metric("arrow.up",   "输出",    Fmt.tokens(card.output),      T.coreViolet)
            metric("bolt.fill",  "缓存读",  Fmt.tokens(card.cacheRead),   T.healthyMint)
            metric("tray.fill",  "缓存写",  Fmt.tokens(card.cacheWrite),  T.warningAmber)
            metric("number",     "请求",    "\(card.requests)",          T.textSecondary)
            metric("percent",    "缓存命中", Fmt.percent(card.cacheRatio * 100), T.textSecondary)
        }
    }

    private func metric(_ icon: String, _ label: String, _ value: String, _ tint: Color) -> some View {
        HStack(spacing: 6) {
            MetricIcon(systemName: icon, tint: tint)
            VStack(alignment: .leading, spacing: 1) {
                Text(label).font(.system(size: 9.5)).foregroundStyle(T.textTertiary)
                Text(value)
                    .font(.system(size: 12, weight: .semibold, design: .monospaced))
                    .foregroundStyle(T.textPrimary)
                    .lineLimit(1)
            }
            Spacer(minLength: 0)
        }
    }

    private var subDisclosure: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text("\(subTitle) (\(card.subs.count))")
                .font(.system(size: 10, weight: .semibold))
                .foregroundStyle(T.textTertiary)
            ForEach(card.subs) { s in
                HStack(spacing: 8) {
                    Text(s.name)
                        .font(.system(size: 11))
                        .foregroundStyle(T.textSecondary)
                        .lineLimit(1)
                    Spacer()
                    Text(Fmt.tokens(s.tokens))
                        .font(.system(size: 11, design: .monospaced))
                        .foregroundStyle(T.textPrimary)
                    Text(Fmt.percent(s.cacheRatio * 100))
                        .font(.system(size: 10, design: .monospaced))
                        .foregroundStyle(T.textTertiary)
                        .frame(width: 38, alignment: .trailing)
                }
            }
        }
        .padding(.top, 2)
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
