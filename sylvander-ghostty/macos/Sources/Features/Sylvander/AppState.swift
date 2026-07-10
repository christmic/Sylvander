// AppState.swift
// 进程级 singleton,持有 SylvanderClient、SessionStore、KeyBindingsStore。
// 任何 view 都能通过 @EnvironmentObject 拿到。

import Foundation
import Combine

@MainActor
final class AppState: ObservableObject {
    let client: SylvanderClient
    let sessions: SessionStore
    let keyBindings: KeyBindingsStore

    @Published var connectionState: SylvanderClient.ClientState = .disconnected
    @Published var currentModel: String = "claude-sonnet-5"
    @Published var tokenUsage: (used: Int, total: Int) = (1200, 200_000)

    init() {
        self.client = SylvanderClient()
        self.sessions = SessionStore()
        self.keyBindings = KeyBindingsStore()
    }

    /// 启动时调用:连 client + 加载历史 session。
    func bootstrap() async {
        await client.connect()
        await sessions.loadInitial()
        connectionState = await client.state
    }

    /// 用户在 composer 回车时调用:发 user message + 拿 agent 流式回复。
    func sendUserMessage(_ text: String) async {
        guard !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return }
        sessions.appendComponent(.user(id: UUID(), text: text))

        let stream = await client.send(prompt: text)
        for await component in stream {
            sessions.appendComponent(component)
        }
    }

    /// 状态栏显示用。
    var connectionLabel: String {
        switch connectionState {
        case .disconnected: return "⚪ disconnected"
        case .connecting: return "🟡 connecting..."
        case .connected: return "🟢 connected"
        }
    }
}