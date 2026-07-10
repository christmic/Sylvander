// StatusBarView.swift
// 底部状态栏:连接状态 / 模型 / token 用量。

import SwiftUI

struct StatusBarView: View {
    @EnvironmentObject var appState: AppState

    var body: some View {
        HStack(spacing: 12) {
            Text(appState.connectionLabel)
                .font(.system(size: 11))

            Text("·")
                .foregroundStyle(.secondary)
                .font(.system(size: 11))

            Text("model: \(appState.currentModel)")
                .font(.system(size: 11))
                .foregroundStyle(.secondary)

            Spacer()

            Text("tokens \(formatNumber(appState.tokenUsage.used))/\(formatNumber(appState.tokenUsage.total))")
                .font(.system(size: 11, design: .monospaced))
                .foregroundStyle(.secondary)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 4)
        .background(
            Rectangle()
                .fill(Color(nsColor: .windowBackgroundColor).opacity(0.6))
        )
        .overlay(
            Rectangle()
                .frame(height: 1)
                .foregroundStyle(Color.secondary.opacity(0.2)),
            alignment: .top
        )
    }

    private func formatNumber(_ n: Int) -> String {
        if n >= 1000 {
            return String(format: "%.1fk", Double(n) / 1000.0)
        }
        return String(n)
    }
}