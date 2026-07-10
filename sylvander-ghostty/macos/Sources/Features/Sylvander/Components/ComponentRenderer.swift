// ComponentRenderer.swift
// 按 Component.type 分发到对应 SwiftUI View。

import SwiftUI

struct ComponentRenderer: View {
    let component: Component

    var body: some View {
        switch component {
        case .markdown(_, let text):
            MarkdownView(text: text)
        case .toolCall(_, let name, let args):
            ToolCallView(name: name, args: args)
        case .toolResult(_, let content):
            ToolResultView(content: content)
        case .actionRow(_, let options):
            ActionRowView(options: options)
        case .user(_, let text):
            UserMessageView(text: text)
        }
    }
}