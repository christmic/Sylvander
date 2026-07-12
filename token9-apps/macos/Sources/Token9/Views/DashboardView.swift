import SwiftUI

struct DashboardView: View {
    @StateObject private var vm = DashboardViewModel()
    @AppStorage("themeMode") private var themeRaw = ThemeMode.warm.rawValue

    private var mode: ThemeMode { ThemeMode(rawValue: themeRaw) ?? .warm }

    var body: some View {
        ZStack {
            VisualEffect().ignoresSafeArea()
            Theme.bg(mode).ignoresSafeArea()

            VStack(alignment: .leading, spacing: 12) {
                topBar
                SegmentedTabs(sel: $vm.range)
                groupBar
                content
            }
            .padding(Theme.outerPad)
        }
        .frame(width: 440, height: 620)
        .onAppear { vm.start() }
        .onDisappear { vm.stop() }
    }

    private var topBar: some View {
        HStack(alignment: .center, spacing: 9) {
            Image(systemName: "timer")
                .font(.system(size: 15, weight: .bold))
                .foregroundStyle(Theme.accent(mode))
            VStack(alignment: .leading, spacing: 0) {
                Text("token9").font(.system(size: 15, weight: .bold)).foregroundStyle(Theme.tPrimary)
                Text("本地 AI 用量").font(.system(size: 9.5)).foregroundStyle(Theme.tTertiary)
            }
            Spacer()
            if let at = vm.updatedAt {
                Text(timeString(at)).font(.system(size: 9.5, design: .monospaced))
                    .foregroundStyle(Theme.tTertiary)
            }
            IconButton(icon: mode.icon) {
                withAnimation(.easeOut(duration: 0.25)) { themeRaw = mode.next.rawValue }
            }
            IconButton(icon: "arrow.clockwise") { vm.reload() }
        }
    }

    private var groupBar: some View {
        HStack(spacing: 8) {
            Text("汇总维度").font(.system(size: 10)).foregroundStyle(Theme.tTertiary)
            Picker("", selection: $vm.groupBy) {
                ForEach(GroupBy.allCases) { g in Text(g.label).tag(g) }
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            .frame(width: 150)
            Spacer()
        }
    }

    @ViewBuilder
    private var content: some View {
        if let e = vm.error, vm.cards.isEmpty {
            errorState(e)
        } else if vm.cards.isEmpty && !vm.loading {
            emptyState
        } else {
            ScrollView {
                VStack(alignment: .leading, spacing: 13) {
                    if vm.cards.count > 1 {
                        VStack(alignment: .leading, spacing: 6) {
                            Text("分布").font(.system(size: 10)).foregroundStyle(Theme.tTertiary)
                            DistributionChart(cards: vm.cards)
                        }
                    }
                    ForEach(vm.cards) { card in
                        GroupCardView(card: card, subTitle: vm.groupBy.subTitle)
                    }
                }
                .padding(.bottom, 6)
            }
        }
    }

    private var emptyState: some View {
        VStack(spacing: 8) {
            Image(systemName: "chart.bar.doc.horizontal")
                .font(.system(size: 30)).foregroundStyle(Theme.tTertiary)
            Text("暂无数据").font(.system(size: 13, weight: .medium)).foregroundStyle(Theme.tSecondary)
            Text("让工具走 token9 (127.0.0.1:9527) 后即可看到用量")
                .font(.system(size: 10)).foregroundStyle(Theme.tTertiary).multilineTextAlignment(.center)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private func errorState(_ e: String) -> some View {
        VStack(spacing: 8) {
            Image(systemName: "exclamationmark.triangle.fill")
                .font(.system(size: 26)).foregroundStyle(.orange)
            Text("连接失败").font(.system(size: 13, weight: .medium)).foregroundStyle(Theme.tSecondary)
            Text("token9 serve 在 :9527 运行吗？").font(.system(size: 10)).foregroundStyle(Theme.tTertiary)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private func timeString(_ d: Date) -> String {
        let f = DateFormatter(); f.dateFormat = "HH:mm:ss"; return f.string(from: d)
    }
}
