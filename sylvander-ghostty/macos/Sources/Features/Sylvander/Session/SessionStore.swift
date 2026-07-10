// SessionStore.swift
// 本地缓存 + mock 后端。F3 接入真 WSS server 时替换 client.sendMessage 实现。

import Foundation
import Combine

@MainActor
final class SessionStore: ObservableObject {
    @Published private(set) var sessions: [Session] = []
    @Published var activeSessionId: String?

    func loadInitial() async {
        // F2: mock 三个历史 session
        let now = Date()
        sessions = [
            Session(
                id: "s-001",
                title: "修复登录 bug",
                lastActive: now.addingTimeInterval(-3600),
                components: [
                    .user(id: UUID(), text: "昨天那个登录 bug 进展?代码在 ~/code/myapp"),
                    .markdown(id: UUID(), text: "我看一下..."),
                    .toolCall(id: UUID(), name: "Read", args: "src/auth/session.rs"),
                    .toolResult(id: UUID(), content: "let session = Session::new();\nself.save(session).await?;"),
                    .markdown(id: UUID(), text: "## 问题分析\n这里有个 race condition。"),
                    .actionRow(id: UUID(), options: ["应用补丁", "看完整 diff", "先别动"]),
                ]
            ),
            Session(
                id: "s-002",
                title: "重构 API",
                lastActive: now.addingTimeInterval(-10800),
                components: [
                    .user(id: UUID(), text: "把 /v1/users 接口拆成 query / mutation"),
                    .markdown(id: UUID(), text: "好的,我来拆分。"),
                ]
            ),
            Session(
                id: "s-003",
                title: "写 README",
                lastActive: now.addingTimeInterval(-86400),
                components: [
                    .user(id: UUID(), text: "帮我的 Rust 项目写个 README"),
                    .markdown(id: UUID(), text: "我先看看 Cargo.toml。"),
                ]
            ),
        ]
        activeSessionId = sessions.first?.id
    }

    func activeSession() -> Session? {
        guard let id = activeSessionId else { return nil }
        return sessions.first { $0.id == id }
    }

    func appendComponent(_ component: Component) {
        guard let id = activeSessionId,
              let idx = sessions.firstIndex(where: { $0.id == id }) else { return }
        sessions[idx].components.append(component)
        sessions[idx].lastActive = Date()
    }

    func createNewSession() async -> Session {
        let new = Session(
            id: "s-\(UUID().uuidString.prefix(8))",
            title: "新建会话",
            lastActive: Date(),
            components: []
        )
        sessions.append(new)
        activeSessionId = new.id
        return new
    }

    func closeSession(_ sessionId: String) {
        // 关 tab = 关 view;session 在内存仍保留,后续可重新激活
        // F3 接 server 后,这里调 server API 标记 closed 但不删
        if activeSessionId == sessionId {
            activeSessionId = sessions.first(where: { $0.id != sessionId })?.id
        }
    }

    func select(_ sessionId: String) {
        activeSessionId = sessionId
    }
}