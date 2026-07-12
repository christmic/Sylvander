import SwiftUI

/// Dashboard v2 — composes the gateway header, range tabs, summary
/// strip, heatmap, and aggregation rows. Drives the five states from
/// IMPLEMENTATION_CHECKLIST.md §8: loading-without-data, empty-success,
/// offline-no-cache, offline-with-cached-data, and rate-limit warning.
struct DashboardView: View {
    @StateObject private var vm = DashboardViewModel()
    @State private var expandedGroupID: String?
    private var dimensionBinding: Binding<DimensionToggle.Dimension> {
        Binding(
            get: { vm.groupBy == .tool ? .tool : .model },
            set: { vm.groupBy = $0 == .tool ? .tool : .model }
        )
    }

    private var allTotal: Int64 { vm.cards.reduce(0) { $0 + $1.totalTokens } }
    private var largestTotal: Int64 { vm.cards.map(\.totalTokens).max() ?? 0 }

    /// Initial load failed and we have no cached data — show offline
    /// error instead of the row list.
    private var isInitialLoadFailed: Bool {
        vm.error != nil && vm.cards.isEmpty && vm.daily.isEmpty
    }
    /// First successful load returned but with zero buckets.
    private var isEmptySuccess: Bool {
        vm.hasSuccessfulLoad && vm.error == nil && !vm.loading && vm.cards.isEmpty
    }
    /// First-load-in-progress with no data yet.
    private var isInitialLoading: Bool {
        !vm.hasSuccessfulLoad && vm.loading && vm.cards.isEmpty
    }

    var body: some View {
        ZStack {
            VisualEffect().ignoresSafeArea()
            T.bgPrimary.ignoresSafeArea()

            VStack(alignment: .leading, spacing: L.majorGap) {
                header
                RangeTabs(sel: $vm.range)
                if !isInitialLoadFailed && !isInitialLoading && !isEmptySuccess {
                    SummaryStripView(summary: vm.summary)
                    if vm.range.showsHeatmap {
                        ActivityHeatmapView(range: vm.range, daily: vm.daily)
                    }
                    aggregationHeading
                    contentBody
                } else if isInitialLoading || isEmptySuccess {
                    contentBody
                } else {
                    offlineErrorBody
                }
                if let rate = vm.minimumRateLimitPercent, rate <= 15 {
                    RateLimitWarning(remainingPercent: rate)
                }
            }
            .padding(L.outerPad)
            .frame(maxHeight: .infinity, alignment: .top)
        }
        .frame(width: L.popoverW, height: L.popoverH)
        .onAppear { vm.start() }
        .onDisappear { vm.stop() }
    }

    // MARK: Header

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

    // MARK: Content states

    @ViewBuilder
    private var contentBody: some View {
        if isInitialLoading {
            Spacer(minLength: 0)
            initialLoadingState
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            Spacer(minLength: 0)
        } else if isEmptySuccess {
            Spacer(minLength: 0)
            emptySuccessState
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            Spacer(minLength: 0)
        } else {
            rowList
            // Cached-offline caption (retained last successful data).
            if vm.error != nil, let updated = vm.updatedAt {
                Text("上次成功 · \(Fmt.shortDate(updated))")
                    .font(.system(size: 10))
                    .foregroundStyle(T.textTertiary)
                    .frame(maxWidth: .infinity, alignment: .trailing)
            }
        }
    }

    private var initialLoadingState: some View {
        ProgressView()
            .controlSize(.small)
            .progressViewStyle(.circular)
            .tint(T.seedOrange)
    }

    private var emptySuccessState: some View {
        VStack(spacing: 8) {
            Text("等待第一条流量")
                .font(.system(size: 13, weight: .medium))
                .foregroundStyle(T.textSecondary)
            Text("将 AI 工具的 Base URL 指向 127.0.0.1:9527")
                .font(.system(size: 10))
                .foregroundStyle(T.textTertiary)
                .multilineTextAlignment(.center)
        }
        .padding(.horizontal, 20)
    }

    private var offlineErrorBody: some View {
        VStack(spacing: 10) {
            Spacer(minLength: 0)
            VStack(spacing: 8) {
                Image(systemName: "wifi.slash")
                    .font(.system(size: 28))
                    .foregroundStyle(T.warningAmber)
                Text("网关未连接")
                    .font(.system(size: 14, weight: .semibold))
                    .foregroundStyle(T.textPrimary)
                Text("启动 token9 serve 后，这里会自动恢复")
                    .font(.system(size: 11))
                    .foregroundStyle(T.textTertiary)
                    .multilineTextAlignment(.center)
                IconButton(systemName: "arrow.clockwise") { vm.reload() }
                    .padding(.top, 4)
            }
            .frame(maxWidth: .infinity)
            Spacer(minLength: 0)
        }
    }

    // MARK: Row list

    private var rowList: some View {
        ScrollView {
            VStack(spacing: 6) {
                ForEach(vm.cards) { card in
                    GroupRowView(
                        card: card,
                        allTotal: allTotal,
                        largestTotal: largestTotal,
                        subTitle: vm.groupBy.subTitle,
                        isExpanded: expandedGroupID == card.id,
                        onToggle: { toggle(card.id) }
                    )
                }
            }
        }
        .frame(maxHeight: .infinity)
    }

    private func toggle(_ id: String) {
        expandedGroupID = (expandedGroupID == id) ? nil : id
    }
}
