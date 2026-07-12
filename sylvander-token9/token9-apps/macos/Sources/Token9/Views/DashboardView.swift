import SwiftUI

/// Dashboard v2 — stub. Phase A establishes the design system; subsequent
/// phases (B–F) progressively wire data, header, summary, heatmap, rows,
/// and states. This commit renders the new popover chrome (background,
/// logo, frame, range tabs placeholder) so `swift build` exits 0 and the
/// asset commit's `SeedCrabMark` PDF/PNG is reachable through the bundle.
struct DashboardView: View {
    @State private var range: RangeKey = .today

    var body: some View {
        ZStack {
            VisualEffect().ignoresSafeArea()
            T.bgPrimary.ignoresSafeArea()

            VStack(alignment: .leading, spacing: L.majorGap) {
                header
                RangeTabs(sel: $range)
                Panel {
                    Text("Dashboard v2 — design system only")
                        .font(.system(size: 12))
                        .foregroundStyle(T.textSecondary)
                }
                Spacer(minLength: 0)
            }
            .padding(L.outerPad)
        }
        .frame(width: L.popoverW, height: L.popoverH)
    }

    private var header: some View {
        HStack(spacing: 10) {
            Image("SeedCrabMark", bundle: .module)
                .resizable()
                .interpolation(.high)
                .frame(width: L.logoSize, height: L.logoSize)
            VStack(alignment: .leading, spacing: 0) {
                Text("token9").font(.system(size: 15, weight: .bold))
                    .foregroundStyle(T.textPrimary)
                Text("本地 LLM 网关").font(.system(size: 10))
                    .foregroundStyle(T.textTertiary)
            }
            Spacer()
            IconButton(systemName: "arrow.clockwise") {}
        }
    }
}