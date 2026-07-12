import AppKit
import SwiftUI

/// Blurred window background (macOS HUD material).
struct VisualEffect: NSViewRepresentable {
    func makeNSView(context: Context) -> NSVisualEffectView {
        let v = NSVisualEffectView()
        v.material = .hudWindow
        v.blendingMode = .behindWindow
        v.state = .active
        return v
    }
    func updateNSView(_ v: NSVisualEffectView, context: Context) {}
}

/// Centralized design tokens: colors, radii, spacing, text tiers.
enum Theme {
    // per-tool tints
    static let claude = Color(red: 0.92, green: 0.52, blue: 0.40)
    static let codex = Color(red: 0.42, green: 0.68, blue: 0.98)
    static let gemini = Color(red: 0.62, green: 0.52, blue: 0.92)
    static let grok = Color(red: 0.65, green: 0.68, blue: 0.75)
    static let qoder = Color(red: 0.90, green: 0.75, blue: 0.35)
    static let syleander = Color(red: 0.40, green: 0.82, blue: 0.60)
    static let other = Color(red: 0.60, green: 0.62, blue: 0.68)

    static let cardRadius: CGFloat = 16
    static let outerPad: CGFloat = 15

    static let tPrimary = Color.white.opacity(0.97)
    static let tSecondary = Color.white.opacity(0.82)
    static let tTertiary = Color.white.opacity(0.58)

    /// Window background per theme mode.
    static func bg(_ mode: ThemeMode) -> LinearGradient {
        switch mode {
        case .warm:
            return LinearGradient(
                colors: [
                    Color(red: 0.22, green: 0.18, blue: 0.16).opacity(0.94),
                    Color(red: 0.13, green: 0.10, blue: 0.09).opacity(0.96),
                ],
                startPoint: .top, endPoint: .bottom)
        case .cool:
            return LinearGradient(
                colors: [
                    Color(red: 0.16, green: 0.18, blue: 0.23).opacity(0.94),
                    Color(red: 0.09, green: 0.11, blue: 0.15).opacity(0.96),
                ],
                startPoint: .top, endPoint: .bottom)
        }
    }

    /// Neutral accent (title icon, refresh, non-entity chrome) per theme mode.
    static func accent(_ mode: ThemeMode) -> Color {
        mode == .warm ? claude : codex
    }

    /// Map a logical tool label to its accent color.
    static func toolTint(_ label: String) -> Color {
        switch label.lowercased() {
        case let l where l.contains("claude"): return claude
        case let l where l.contains("codex"): return codex
        case let l where l.contains("gemini"): return gemini
        case let l where l.contains("grok"): return grok
        case let l where l.contains("qoder"): return qoder
        case let l where l.contains("syleander") || l.contains("sylvander"): return syleander
        default: return other
        }
    }

    private static let palette: [Color] = [
        claude, codex, gemini, syleander, qoder,
        Color(red: 0.85, green: 0.45, blue: 0.68),  // rose
        Color(red: 0.55, green: 0.75, blue: 0.90),  // sky
        Color(red: 0.74, green: 0.58, blue: 0.95),  // violet
    ]

    /// Deterministic color for an arbitrary name (used for model grouping).
    static func paletteColor(_ name: String) -> Color {
        var h: UInt64 = 1469598103934665603
        for b in name.utf8 { h = (h ^ UInt64(b)) &* 1099511628211 }
        return palette[Int(h % UInt64(palette.count))]
    }
}

/// Two moods: warm (coding energy) and cool (calm, Codex-like).
enum ThemeMode: String, CaseIterable {
    case warm, cool
    var next: ThemeMode { self == .warm ? .cool : .warm }
    var icon: String { self == .warm ? "flame.fill" : "snowflake" }
}

/// Floating card: layered fill + tint wash + top highlight + gradient stroke + soft shadow + hover.
struct Card<Content: View>: View {
    var tint: Color
    @ViewBuilder var content: () -> Content
    @State private var hover = false
    var body: some View {
        content()
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            .padding(15)
            .background(
                RoundedRectangle(cornerRadius: Theme.cardRadius, style: .continuous)
                    .fill(Color.black.opacity(0.26))
                    .overlay(
                        RoundedRectangle(cornerRadius: Theme.cardRadius, style: .continuous)
                            .fill(tint.opacity(0.08))
                    )
                    .overlay(alignment: .top) {
                        RoundedRectangle(cornerRadius: Theme.cardRadius, style: .continuous)
                            .fill(LinearGradient(
                                colors: [Color.white.opacity(0.05), .clear],
                                startPoint: .top, endPoint: .center))
                    }
            )
            .overlay(
                RoundedRectangle(cornerRadius: Theme.cardRadius, style: .continuous)
                    .strokeBorder(
                        LinearGradient(
                            colors: [tint.opacity(0.38), tint.opacity(0.05)],
                            startPoint: .topLeading, endPoint: .bottomTrailing),
                        lineWidth: 0.75)
            )
            .shadow(color: Color.black.opacity(hover ? 0.42 : 0.30),
                    radius: hover ? 16 : 12, x: 0, y: hover ? 9 : 6)
            .scaleEffect(hover ? 1.012 : 1)
            .onHover { hover = $0 }
            .animation(.easeOut(duration: 0.18), value: hover)
    }
}

/// 2-column layout where each row's cards share the tallest height.
struct EqualHeightGrid: Layout {
    var columns = 2
    var hSpacing: CGFloat = 13
    var vSpacing: CGFloat = 13

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        guard !subviews.isEmpty else { return .zero }
        let total = proposal.width ?? 700
        let colW = colWidth(in: total)
        var h: CGFloat = 0
        for row in stride(from: 0, to: subviews.count, by: columns) {
            if row > 0 { h += vSpacing }
            h += rowHeight(row: row, colW: colW, subviews: subviews)
        }
        return CGSize(width: total, height: h)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        let colW = colWidth(in: bounds.width)
        var y = bounds.minY
        for row in stride(from: 0, to: subviews.count, by: columns) {
            let rh = rowHeight(row: row, colW: colW, subviews: subviews)
            for i in row..<min(row + columns, subviews.count) {
                let x = bounds.minX + CGFloat(i - row) * (colW + hSpacing)
                subviews[i].place(at: CGPoint(x: x, y: y), anchor: .topLeading,
                                  proposal: .init(width: colW, height: rh))
            }
            y += rh + vSpacing
        }
    }

    private func colWidth(in total: CGFloat) -> CGFloat {
        (total - hSpacing * CGFloat(columns - 1)) / CGFloat(columns)
    }
    private func rowHeight(row: Int, colW: CGFloat, subviews: Subviews) -> CGFloat {
        (row..<min(row + columns, subviews.count)).map {
            subviews[$0].sizeThatFits(.init(width: colW, height: nil)).height
        }.max() ?? 0
    }
}

/// Big headline number + caption.
struct Headline: View {
    var value: String
    var caption: String
    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 7) {
            Text(value)
                .font(.system(size: 26, weight: .bold, design: .rounded))
                .foregroundStyle(.white)
                .contentTransition(.numericText())
            Text(caption)
                .font(.system(size: 10))
                .foregroundStyle(Theme.tTertiary)
            Spacer(minLength: 0)
        }
    }
}

/// Icon + label + monospaced value.
struct MetricCell: View {
    var icon: String
    var label: String
    var value: String
    var tint: Color
    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: icon)
                .font(.system(size: 9.5, weight: .bold))
                .foregroundStyle(tint)
                .frame(width: 21, height: 21)
                .background(Circle().fill(tint.opacity(0.10)))
            VStack(alignment: .leading, spacing: 1) {
                Text(label).font(.system(size: 9.5)).foregroundStyle(Theme.tTertiary)
                Text(value)
                    .font(.system(size: 12.5, weight: .semibold, design: .monospaced))
                    .foregroundStyle(Theme.tPrimary)
            }
            Spacer(minLength: 0)
        }
    }
}

/// Small ring + label + percentage (cache hit).
struct RingMetricCell: View {
    var value: Double
    var label: String
    var tint: Color
    var body: some View {
        HStack(spacing: 8) {
            ZStack {
                Circle().stroke(Color.primary.opacity(0.10), lineWidth: 2.5)
                Circle()
                    .trim(from: 0, to: max(0.001, min(1, value / 100)))
                    .stroke(tint.gradient, style: StrokeStyle(lineWidth: 2.5, lineCap: .round))
                    .rotationEffect(.degrees(-90))
            }
            .frame(width: 21, height: 21)
            VStack(alignment: .leading, spacing: 1) {
                Text(label).font(.system(size: 9.5)).foregroundStyle(Theme.tTertiary)
                Text("\(Int(value.rounded()))%")
                    .font(.system(size: 12.5, weight: .semibold, design: .monospaced))
                    .foregroundStyle(Theme.tPrimary)
            }
            Spacer(minLength: 0)
        }
        .animation(.easeOut(duration: 0.5), value: value)
    }
}

/// Thin progress bar (rate limits).
struct MiniBar: View {
    var value: Double  // 0...100
    var tint: Color
    var body: some View {
        GeometryReader { geo in
            ZStack(alignment: .leading) {
                Capsule().fill(Color.primary.opacity(0.09))
                Capsule()
                    .fill(tint.gradient)
                    .frame(width: max(3, geo.size.width * min(1, value / 100)))
            }
        }
        .frame(height: 5)
        .animation(.easeOut(duration: 0.45), value: value)
    }
}

/// Sliding segmented control for the time range.
struct SegmentedTabs: View {
    @Binding var sel: RangeKey
    @Namespace private var ns
    var body: some View {
        HStack(spacing: 2) {
            ForEach(RangeKey.allCases) { k in
                let on = k == sel
                Text(k.label)
                    .font(.system(size: 12, weight: on ? .semibold : .regular))
                    .foregroundStyle(on ? AnyShapeStyle(.primary) : AnyShapeStyle(.secondary))
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 5)
                    .background {
                        if on {
                            RoundedRectangle(cornerRadius: 8, style: .continuous)
                                .fill(.ultraThinMaterial)
                                .overlay(
                                    RoundedRectangle(cornerRadius: 8, style: .continuous)
                                        .strokeBorder(Color.primary.opacity(0.14), lineWidth: 1))
                                .shadow(color: .black.opacity(0.18), radius: 3, y: 1)
                                .matchedGeometryEffect(id: "seg", in: ns)
                        }
                    }
                    .contentShape(Rectangle())
                    .onTapGesture {
                        withAnimation(.spring(response: 0.32, dampingFraction: 0.82)) { sel = k }
                    }
            }
        }
        .padding(3)
        .background(RoundedRectangle(cornerRadius: 11, style: .continuous)
            .fill(Color.primary.opacity(0.06)))
    }
}

/// Hover-highlight icon button.
struct IconButton: View {
    var icon: String
    var action: () -> Void
    @State private var hover = false
    var body: some View {
        Button(action: action) {
            Image(systemName: icon)
                .font(.system(size: 12, weight: .semibold))
                .foregroundStyle(hover ? AnyShapeStyle(.primary) : AnyShapeStyle(.secondary))
                .frame(width: 28, height: 24)
                .background(RoundedRectangle(cornerRadius: 7, style: .continuous)
                    .fill(Color.primary.opacity(hover ? 0.10 : 0)))
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .onHover { hover = $0 }
    }
}

/// Small rounded pill badge.
struct Pill: View {
    var text: String
    var tint: Color
    var body: some View {
        Text(text)
            .font(.system(size: 10, weight: .semibold, design: .monospaced))
            .foregroundStyle(tint)
            .padding(.horizontal, 6)
            .padding(.vertical, 2)
            .background(Capsule().fill(tint.opacity(0.15)))
    }
}
