// EmptyStateView.swift
// 没有历史 session 时的引导:告诉用户怎么新建。

import SwiftUI

struct EmptyStateView: View {
    @EnvironmentObject var appState: AppState

    var body: some View {
        VStack(spacing: 16) {
            Image(systemName: "bubble.left.and.bubble.right")
                .font(.system(size: 56, weight: .light))
                .foregroundStyle(.secondary)

            Text("还没有任何会话")
                .font(.title2)
                .foregroundStyle(.primary)

            VStack(spacing: 6) {
                Text("点 [+] 或按 ⇧⌘T")
                Text("开始跟你的第一个 agent 对话")
            }
            .font(.callout)
            .foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}