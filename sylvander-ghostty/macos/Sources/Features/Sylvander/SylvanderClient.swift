// SylvanderClient.swift
// F2 阶段:完全 mock,本地假数据。
// F3 阶段:替换为 URLSessionWebSocketTask 真接 WSS。

import Foundation

actor SylvanderClient {
    enum ClientState: Equatable {
        case disconnected
        case connecting
        case connected
    }

    private(set) var state: ClientState = .disconnected

    /// 流式产生 component 序列。
    /// F2:本地假数据。F3:从 WSS 收消息,按 type 映射成 Component。
    func send(prompt: String) -> AsyncStream<Component> {
        AsyncStream { continuation in
            Task {
                continuation.yield(.markdown(
                    id: UUID(),
                    text: "你说:\n\n> \(prompt)"
                ))

                try? await Task.sleep(nanoseconds: 400_000_000)

                continuation.yield(.markdown(
                    id: UUID(),
                    text: "好的,我来帮你看看。"
                ))

                try? await Task.sleep(nanoseconds: 400_000_000)

                continuation.yield(.toolCall(
                    id: UUID(),
                    name: "Bash",
                    args: "ls -la ~/code/myapp/src/auth/"
                ))

                try? await Task.sleep(nanoseconds: 400_000_000)

                continuation.yield(.toolResult(
                    id: UUID(),
                    content: "total 24\ndrwxr-xr-x  5 user  staff   160 Jul  9 10:00 .\ndrwxr-xr-x  3 user  staff    96 Jul  9 09:55 ..\n-rw-r--r--  1 user  staff  1024 Jul  9 10:00 login.rs\n-rw-r--r--  1 user  staff  2048 Jul  9 10:00 session.rs"
                ))

                try? await Task.sleep(nanoseconds: 400_000_000)

                continuation.yield(.markdown(
                    id: UUID(),
                    text: "## 诊断结果\n\n`session.rs:42` 存在 race condition。`Session::new()` 返回前未 await,但 `self.save()` 已开始。\n\n建议:\n1. 把 `Session::new()` 改成 async\n2. 或者在 save 之前加锁"
                ))

                try? await Task.sleep(nanoseconds: 400_000_000)

                continuation.yield(.actionRow(
                    id: UUID(),
                    options: ["应用补丁", "看完整 diff", "先别动"]
                ))

                continuation.finish()
            }
        }
    }

    func connect() async {
        state = .connecting
        try? await Task.sleep(nanoseconds: 200_000_000)
        state = .connected
    }

    func disconnect() {
        state = .disconnected
    }
}