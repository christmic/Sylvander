// Component.swift
// Sylvander tab 里消息流的最小渲染单元。
// 服务端按 component 序列推送;客户端按 type 分发到对应 SwiftUI View。

import Foundation

/// 一条消息(单轮对话)由若干 component 顺序组成。
enum Component: Identifiable, Equatable, Hashable {
    case markdown(id: UUID, text: String)
    case toolCall(id: UUID, name: String, args: String)
    case toolResult(id: UUID, content: String)
    case actionRow(id: UUID, options: [String])
    case user(id: UUID, text: String)

    var id: UUID {
        switch self {
        case .markdown(let id, _),
             .toolCall(let id, _, _),
             .toolResult(let id, _),
             .actionRow(let id, _),
             .user(let id, _):
            return id
        }
    }
}