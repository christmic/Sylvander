import SwiftUI

/// Dashboard v2 — Phase B renders the gateway header + range tabs +
/// the summary strip driven by the new `DashboardSummary` aggregation.
/// Heatmap + aggregation rows land in commits 4 and 5 respectively.
struct DashboardView: View {
    @StateObject private var vm = DashboardViewModel()
    private var dimensionBinding: Binding<DimensionToggle.Dimension> {
        Binding(
            get: { vm.groupBy == .tool ? .tool : .model },
            set: { vm.groupBy = $0 == .tool ? .tool : .model }
        )
    }

    var body: some View {
        ZStack {
            VisualEffect().ignoresSafeArea()
            T.bgPrimary.ignoresSafeArea()

            VStack(alignment: .leading, spacing: L.majorGap) {
                header
                RangeTabs(sel: $vm.range)
                SummaryStripView(summary: vm.summary)
                if vm.range.showsHeatmap {
                    ActivityHeatmapView(range: vm.range, daily: vm.daily)
                }
                aggregationHeading
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
                Text(vm.error == nil ? "在线 · 127.0.0.1:9527" : "离线 · 127.0.0.1:9527")
                    .font(.system(size: 10)).foregroundStyle(T.textTertiary)
            }
            IconButton(systemName: "arrow.clockwise") { vm.reload() }
        }
    }

    private var aggregationHeading: some View {
        HStack(alignment: .center, spacing: 10) {
            Text("汇总维度")
                .font(.system(size: 11))
                .foregroundStyle(T.textTertiary)
            DimensionToggle(sel: dimensionBinding)
            Spacer()
        }
    }
}