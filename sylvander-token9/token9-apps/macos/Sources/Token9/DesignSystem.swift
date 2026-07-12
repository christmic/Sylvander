import AppKit
import SwiftUI

// MARK: - Visual effect (kept from v1)

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

// MARK: - Design tokens

/// Color tokens. Single source of truth for the dashboard v2 palette.
enum T {
    // Backgrounds
    static let bgPrimary      = Color(red: 0.067, green: 0.075, blue: 0.098)  // #111319
    static let bgElevated     = Color.white.opacity(0.05)
    static let borderSubtle   = Color.white.opacity(0.08)

    // Brand
    static let seedOrange     = Color(red: 0.945, green: 0.541, blue: 0.404)  // #F18A67
    static let seedOrangeDeep = Color(red: 0.784, green: 0.353, blue: 0.239)  // #C85A3D
    static let coreViolet     = Color(red: 0.463, green: 0.341, blue: 0.839)  // #7657D6
    static let electricBlue   = Color(red: 0.263, green: 0.529, blue: 0.898)  // #4387E5
    static let healthyMint    = Color(red: 0.345, green: 0.831, blue: 0.608)  // #58D49B
    static let warningAmber   = Color.orange

    // Text tiers
    static let textPrimary    = Color.white.opacity(0.98)
    static let textSecondary  = Color.white.opacity(0.73)
    static let textTertiary   = Color.white.opacity(0.53)

    // Heatmap 5-level scale (graphite → seed orange). Public for the
    // ActivityHeatmapView's pure-function tests.
    static let heatmapLevels: [Color] = [
        Color(red: 0.18, green: 0.20, blue: 0.24),                              // graphite
        Color(red: 0.18, green: 0.12, blue: 0.32),                              // deep violet
        Color(red: 0.46, green: 0.34, blue: 0.84),                              // core violet
        Color(red: 0.26, green: 0.53, blue: 0.90),                              // electric blue
        Color(red: 0.95, green: 0.54, blue: 0.40),                              // seed orange
    ]
}

/// Layout constants. Per IMPLEMENTATION_CHECKLIST.md §3 A2.
enum L {
    static let popoverW:  CGFloat = 480
    static let popoverH:  CGFloat = 660
    static let outerPad:  CGFloat = 16
    static let majorGap:  CGFloat = 14
    static let cardRadius:CGFloat = 12
    static let rowRadius: CGFloat = 12
    static let hairline:  CGFloat = 0.75

    static let logoSize:  CGFloat = 34
    static let rowMinHit: CGFloat = 44
}

// MARK: - Primitives

/// Small filled circle: online/offline indicator.
struct StatusDot: View {
    var active: Bool
    var body: some View {
        Circle()
            .fill(active ? T.healthyMint : T.warningAmber)
            .frame(width: 8, height: 8)
    }
}

/// Plain icon button — hover changes opacity, never scales.
struct IconButton: View {
    var systemName: String
    var action: () -> Void
    @State private var hover = false
    var body: some View {
        Button(action: action) {
            Image(systemName: systemName)
                .font(.system(size: 12, weight: .semibold))
                .foregroundStyle(hover ? T.textPrimary : T.textSecondary)
                .frame(width: 28, height: 24)
                .background(
                    RoundedRectangle(cornerRadius: 7, style: .continuous)
                        .fill(hover ? Color.white.opacity(0.08) : .clear)
                )
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .onHover { hover = $0 }
    }
}

/// Elevated surface: subtle fill + hairline border, no shadow, no scale.
struct Panel<Content: View>: View {
    var radius: CGFloat = L.cardRadius
    @ViewBuilder var content: () -> Content
    var body: some View {
        content()
            .padding(14)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(
                RoundedRectangle(cornerRadius: radius, style: .continuous)
                    .fill(T.bgElevated)
            )
            .overlay(
                RoundedRectangle(cornerRadius: radius, style: .continuous)
                    .strokeBorder(T.borderSubtle, lineWidth: L.hairline)
            )
    }
}

/// Small circular icon background + tinted glyph. Used in metric cells.
struct MetricIcon: View {
    var systemName: String
    var tint: Color
    var body: some View {
        Image(systemName: systemName)
            .font(.system(size: 10, weight: .bold))
            .foregroundStyle(tint)
            .frame(width: 22, height: 22)
            .background(Circle().fill(tint.opacity(0.15)))
    }
}

/// Thin progress bar (rate limits, cache, share). Capsule, 5pt high.
struct MiniBar: View {
    var value: Double   // 0...100
    var tint: Color
    var body: some View {
        GeometryReader { geo in
            ZStack(alignment: .leading) {
                Capsule().fill(Color.white.opacity(0.08))
                Capsule()
                    .fill(tint)
                    .frame(width: max(2, geo.size.width * min(1, max(0, value) / 100)))
            }
        }
        .frame(height: 5)
        .animation(.easeOut(duration: 0.45), value: value)
    }
}

/// Ring-shaped cache hit % indicator.
struct CacheRing: View {
    var value: Double   // 0...100
    var tint: Color = T.seedOrange
    var lineWidth: CGFloat = 3
    var body: some View {
        ZStack {
            Circle().stroke(Color.white.opacity(0.10), lineWidth: lineWidth)
            Circle()
                .trim(from: 0, to: max(0.001, min(1, value / 100)))
                .stroke(tint, style: StrokeStyle(lineWidth: lineWidth, lineCap: .round))
                .rotationEffect(.degrees(-90))
        }
        .animation(.easeOut(duration: 0.5), value: value)
    }
}

/// Time-range tabs. Six cases in order: yesterday / today / week /
/// lastWeek / month / year. Selected tab uses a seed-orange underline,
/// not a full-width pill (per checklist §3 A3 DimensionToggle parallel —
/// the range tabs follow the same restrained pattern).
struct RangeTabs: View {
    @Binding var sel: RangeKey
    var body: some View {
        HStack(spacing: 0) {
            ForEach(RangeKey.allCases) { k in
                let on = k == sel
                VStack(spacing: 4) {
                    Text(k.label)
                        .font(.system(size: 12, weight: on ? .semibold : .regular))
                        .foregroundStyle(on ? T.textPrimary : T.textSecondary)
                    Rectangle()
                        .fill(on ? T.seedOrange : .clear)
                        .frame(height: 2)
                }
                .frame(maxWidth: .infinity)
                .frame(height: 36)
                .contentShape(Rectangle())
                .onTapGesture { sel = k }
            }
        }
    }
}

/// Tool/model dimension toggle. Two text labels with seed-orange
/// underline on the selected one. **Not** a segmented control.
struct DimensionToggle: View {
    enum Dimension: String, CaseIterable, Identifiable {
        case tool = "工具"
        case model = "模型"
        var id: String { rawValue }
    }
    @Binding var sel: Dimension
    var body: some View {
        HStack(spacing: 18) {
            ForEach(Dimension.allCases) { d in
                let on = d == sel
                VStack(spacing: 3) {
                    Text(d.rawValue)
                        .font(.system(size: 11, weight: on ? .semibold : .regular))
                        .foregroundStyle(on ? T.seedOrange : T.textTertiary)
                    Rectangle()
                        .fill(on ? T.seedOrange : .clear)
                        .frame(height: 2)
                        .frame(maxWidth: d.rawValue.count > 2 ? 28 : 18)
                }
                .contentShape(Rectangle())
                .onTapGesture { sel = d }
            }
        }
    }
}