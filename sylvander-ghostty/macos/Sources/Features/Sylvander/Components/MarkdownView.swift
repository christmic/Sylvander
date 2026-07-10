// MarkdownView.swift
// F2 阶段:用 AttributedString 处理基本 markdown(bold / italic / inline code / 标题)。
// F4 阶段:替换为 SwiftMarkdown 或 Down 库。

import SwiftUI

struct MarkdownView: View {
    let text: String

    var body: some View {
        Text(attributed)
            .textSelection(.enabled)
            .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var attributed: AttributedString {
        // 简化:SwiftUI 自带 AttributedString 解析 markdown 子集
        if let attr = try? AttributedString(
            markdown: text,
            options: AttributedString.MarkdownParsingOptions(
                interpretedSyntax: .inlineOnlyPreservingWhitespace
            )
        ) {
            return attr
        }
        return AttributedString(text)
    }
}