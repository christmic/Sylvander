// PreferencesController.swift
// 偏好设置窗口:目前只有快捷键一项。F3+ 加 server / workspace / 模型选择。

import Cocoa
import SwiftUI

final class PreferencesController: NSWindowController {
    init(appState: AppState) {
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 420, height: 240),
            styleMask: [.titled, .closable],
            backing: .buffered,
            defer: false
        )
        window.title = "Sylvander 偏好设置"
        window.center()

        super.init(window: window)
        window.contentView = NSHostingView(
            rootView: PreferencesView()
                .environmentObject(appState)
        )
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }
}

struct PreferencesView: View {
    @EnvironmentObject var appState: AppState
    @State private var recordingForNewTab = false

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text("键盘快捷键")
                .font(.title3.weight(.semibold))

            HStack {
                Text("新建 Sylvander tab:")
                    .frame(width: 180, alignment: .leading)

                Button(action: { recordingForNewTab.toggle() }) {
                    Text(recordingForNewTab
                         ? "请按键…"
                         : appState.keyBindings.newSylvanderTab.displayString)
                        .frame(minWidth: 100)
                        .padding(.horizontal, 10)
                        .padding(.vertical, 4)
                }
                .background(
                    RoundedRectangle(cornerRadius: 4)
                        .stroke(Color.secondary.opacity(0.4), lineWidth: 1)
                )
                .buttonStyle(.plain)
            }

            HStack {
                Button("恢复默认") {
                    appState.keyBindings.newSylvanderTab = .defaultNewTab
                    appState.keyBindings.save()
                }
                Spacer()
                Button("保存") {
                    appState.keyBindings.save()
                    NSApp.keyWindow?.close()
                }
                .keyboardShortcut(.defaultAction)
            }
        }
        .padding(20)
        .frame(width: 420, height: 240)
        .onAppear {
            appState.keyBindings.load()
        }
    }
}

extension Notification.Name {
    static let sylvanderOpenPreferences = Notification.Name("sylvanderOpenPreferences")
}