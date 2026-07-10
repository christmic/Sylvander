// SessionTabBar.swift
// 右侧纵向 tab bar,显示所有 session。
// 点 tab 切到对应 session 内容;点 + 新建;点 ⚙ 进 Preferences。

import SwiftUI

struct SessionTabBar: View {
    @EnvironmentObject var appState: AppState

    var body: some View {
        VStack(spacing: 0) {
            if appState.sessions.sessions.isEmpty {
                Spacer()
            } else {
                ScrollView {
                    VStack(spacing: 2) {
                        ForEach(appState.sessions.sessions) { session in
                            SessionTabItem(session: session)
                        }
                    }
                    .padding(.vertical, 6)
                }
            }

            Divider()

            // 底部操作区
            HStack(spacing: 12) {
                Button(action: createNew) {
                    Image(systemName: "plus")
                        .font(.system(size: 16, weight: .medium))
                }
                .buttonStyle(.plain)
                .help("新建会话 (⇧⌘T)")

                Button(action: openPreferences) {
                    Image(systemName: "gearshape")
                        .font(.system(size: 16, weight: .medium))
                }
                .buttonStyle(.plain)
                .help("偏好设置")
            }
            .padding(.vertical, 10)
            .frame(maxWidth: .infinity)
        }
        .frame(width: 180)
        .background(
            Rectangle()
                .fill(Color(nsColor: .windowBackgroundColor).opacity(0.5))
        )
        .overlay(
            Rectangle()
                .frame(width: 1)
                .foregroundStyle(Color.secondary.opacity(0.2)),
            alignment: .leading
        )
    }

    private func createNew() {
        Task { await appState.sessions.createNewSession() }
    }

    private func openPreferences() {
        NotificationCenter.default.post(
            name: .sylvanderOpenPreferences,
            object: nil
        )
    }
}

struct SessionTabItem: View {
    let session: Session
    @EnvironmentObject var appState: AppState

    var isActive: Bool {
        appState.sessions.activeSessionId == session.id
    }

    var body: some View {
        HStack(spacing: 6) {
            Circle()
                .fill(isActive ? Color.accentColor : Color.secondary.opacity(0.4))
                .frame(width: 6, height: 6)

            Text(session.title)
                .font(.system(size: 12))
                .lineLimit(1)
                .foregroundStyle(isActive ? .primary : .secondary)
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 6)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: 4)
                .fill(isActive ? Color.accentColor.opacity(0.15) : Color.clear)
        )
        .contentShape(Rectangle())
        .onTapGesture {
            appState.sessions.select(session.id)
        }
        .contextMenu {
            Button("关闭 tab") {
                appState.sessions.closeSession(session.id)
            }
        }
    }
}