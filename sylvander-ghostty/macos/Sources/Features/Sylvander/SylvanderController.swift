// SylvanderController.swift
// 主窗口 controller:装起 HStack(Sidebar + Main Area)+ Main Area 里是 VStack(Stream + Composer + StatusBar)。
// 接受外部传入的 AppState(进程级单例),保证多个窗口共享同一状态。

import Cocoa
import SwiftUI

final class SylvanderController: NSWindowController {
    init(appState: AppState) {
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 900, height: 600),
            styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
            backing: .buffered,
            defer: false
        )
        window.title = "Sylvander"
        window.minSize = NSSize(width: 600, height: 400)
        window.center()

        super.init(window: window)

        window.contentView = NSHostingView(
            rootView: SylvanderRootView()
                .environmentObject(appState)
        )
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError() }
}

struct SylvanderRootView: View {
    @EnvironmentObject var appState: AppState

    var body: some View {
        HStack(spacing: 0) {
            SessionTabBar()

            if appState.sessions.activeSession() == nil {
                EmptyStateView()
            } else {
                MainAreaView()
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

struct MainAreaView: View {
    @EnvironmentObject var appState: AppState

    var body: some View {
        VStack(spacing: 0) {
            if let session = appState.sessions.activeSession() {
                SessionHeaderView(session: session)
                MessageStreamView(session: session)
            }
            ComposerView()
            StatusBarView()
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

struct SessionHeaderView: View {
    let session: Session

    var body: some View {
        HStack {
            Text(session.title)
                .font(.headline)
            Spacer()
            Text(session.lastActive, style: .relative)
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 8)
        .background(
            Rectangle()
                .fill(Color(nsColor: .windowBackgroundColor).opacity(0.6))
        )
        .overlay(
            Rectangle()
                .frame(height: 1)
                .foregroundStyle(Color.secondary.opacity(0.2)),
            alignment: .bottom
        )
    }
}

struct MessageStreamView: View {
    let session: Session
    @EnvironmentObject var appState: AppState

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 12) {
                    ForEach(session.components) { component in
                        ComponentRenderer(component: component)
                            .id(component.id)
                    }
                    // 监听 sessions 变化,把 stream 滚到底
                    Color.clear
                        .frame(height: 1)
                        .id("__bottom__")
                }
                .padding(16)
            }
            .onChange(of: appState.sessions.sessions) { _ in
                withAnimation(.linear(duration: 0.1)) {
                    proxy.scrollTo("__bottom__", anchor: .bottom)
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Color(nsColor: .textBackgroundColor))
    }
}