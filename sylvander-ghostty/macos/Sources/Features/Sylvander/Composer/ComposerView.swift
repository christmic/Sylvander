// ComposerView.swift
// 底部输入框。回车发送,⇧回车换行。

import SwiftUI

struct ComposerView: View {
    @EnvironmentObject var appState: AppState
    @State private var text: String = ""
    @FocusState private var focused: Bool

    var body: some View {
        HStack(alignment: .bottom, spacing: 8) {
            Image(systemName: "bubble.left")
                .foregroundStyle(.secondary)
                .font(.system(size: 14))

            TextField("继续输入...", text: $text, axis: .vertical)
                .textFieldStyle(.plain)
                .focused($focused)
                .lineLimit(1...6)
                .onSubmit(send)

            Button(action: send) {
                Image(systemName: "arrow.up.circle.fill")
                    .font(.system(size: 22))
            }
            .buttonStyle(.plain)
            .disabled(text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            .foregroundStyle(text.isEmpty ? Color.secondary : Color.accentColor)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(
            Rectangle()
                .fill(Color(nsColor: .textBackgroundColor))
        )
        .overlay(
            Rectangle()
                .frame(height: 1)
                .foregroundStyle(Color.secondary.opacity(0.2)),
            alignment: .top
        )
    }

    private func send() {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        let toSend = text
        text = ""
        Task { await appState.sendUserMessage(toSend) }
    }
}