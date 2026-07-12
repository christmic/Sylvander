import SwiftUI

/// Dashboard v2 — Phase B renders the gateway header + range tabs +
/// the summary strip driven by the new `DashboardSummary` aggregation.
/// Heatmap + aggregation rows land in commits 4 and 5 respectively.
struct DashboardView: View {
    @StateObject private var vm = DashboardViewModel()
    @State private var dimension: DimensionToggle.Dimension = .tool

    var body: some View {
        ZStack {
            VisualEffect().ignoresSafeArea()
            T.bgPrimary.ignoresSafeArea()

            VStack(alignment: .leading, spacing: L.majorGap) {
                header
                RangeTabs(sel: $vm.range)
                SummaryStripView(summary: vm.summary)
                Panel {
                    HStack(spacing: 12) {
                        Text("日用量（Phase D 接入热力图）")
                            .font(.system(size: 12)).foregroundStyle(T.textSecondary)
                        Spacer()
                        Text("\(vm.daily.count) 天")
                            .font(.system(size: 11, design: .monospaced))
                            .foregroundStyle(T.textTertiary)
                    }
                }
                Spacer(minLength: 0)
            }
            .padding(L.outerPad)
        }
        .frame(width: L.popoverW, height: L.popoverH)
        .onAppear { vm.start() }
        .onDisappear { vm.stop() }
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
            HStack(spacing: 6) {
                StatusDot(active: vm.error == nil)
                Text(vm.error == nil ? "在线" : "离线")
                    .font(.system(size: 10)).foregroundStyle(T.textTertiary)
            }
            IconButton(systemName: "arrow.clockwise") { vm.reload() }
        }
    }
}