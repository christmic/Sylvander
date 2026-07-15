#if os(macOS)
import SwiftUI

struct SylvanderSessionSidebar: View {
    @ObservedObject var store: SylvanderSessionStore

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            search
            status

            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0) {
                    sessionSection("CONTINUE", sessions: continuingSessions)
                    sessionSection("RECENT", sessions: recentSessions)
                }
                .padding(.top, 10)
            }
        }
        .frame(width: 276)
        .background(
            LinearGradient(
                colors: [SylvanderWorkspacePalette.canvas, SylvanderWorkspacePalette.panel],
                startPoint: .topLeading,
                endPoint: .bottomTrailing
            )
        )
        .foregroundStyle(SylvanderWorkspacePalette.text)
    }

    private var header: some View {
        HStack {
            Text("SESSIONS")
                .font(.system(size: 10, weight: .bold, design: .monospaced))
                .tracking(1.2)
                .foregroundStyle(SylvanderWorkspacePalette.dim)
            Spacer()
            Button(action: store.refresh) {
                Image(systemName: "arrow.clockwise")
                    .font(.system(size: 11, weight: .medium))
                    .foregroundStyle(SylvanderWorkspacePalette.dim)
            }
            .buttonStyle(.plain)
            .help("Refresh sessions")
        }
        .padding(.horizontal, 18)
        .padding(.top, 12)
        .frame(height: 54)
        .overlay(alignment: .bottom) { Rectangle().fill(SylvanderWorkspacePalette.rule).frame(height: 1) }
    }

    private var search: some View {
        HStack(spacing: 8) {
            Image(systemName: "magnifyingglass")
                .font(.system(size: 10, weight: .medium))
                .foregroundStyle(SylvanderWorkspacePalette.muted)
            TextField("FILTER SESSIONS", text: $store.query)
                .textFieldStyle(.plain)
                .font(.system(size: 10, weight: .medium, design: .monospaced))
                .foregroundStyle(SylvanderWorkspacePalette.text)
        }
        .padding(.horizontal, 18)
        .frame(height: 52)
        .overlay(alignment: .bottom) {
            Rectangle().fill(SylvanderWorkspacePalette.rule).frame(height: 1).padding(.horizontal, 18)
        }
    }

    @ViewBuilder
    private var status: some View {
        switch store.connectionState {
        case .connecting:
            statusLine("CONNECTING", color: SylvanderWorkspacePalette.idle)
        case .online where store.sessions.isEmpty:
            statusLine("NO SESSIONS", color: SylvanderWorkspacePalette.muted)
        case .online:
            EmptyView()
        case .offline:
            statusLine("SERVER OFFLINE", color: SylvanderWorkspacePalette.signal)
        }
    }

    private func statusLine(_ text: String, color: Color) -> some View {
        HStack(spacing: 8) {
            Circle().fill(color).frame(width: 5, height: 5)
            Text(text)
            Spacer()
        }
        .font(.system(size: 9, weight: .semibold, design: .monospaced))
        .tracking(0.8)
        .foregroundStyle(color)
        .padding(.horizontal, 18)
        .frame(height: 34)
    }

    private func sessionRow(_ session: SylvanderSession) -> some View {
        let selected = store.selectedSessionID == session.id
        return Button {
            withAnimation(.easeOut(duration: 0.16)) {
                store.selectedSessionID = session.id
            }
        } label: {
            HStack(alignment: .top, spacing: 10) {
                Circle()
                    .fill(presenceColor(session.presence))
                    .frame(width: 6, height: 6)
                    .padding(.top, 5)

                VStack(alignment: .leading, spacing: 5) {
                    Text(session.label)
                        .font(.system(size: 12, weight: selected ? .semibold : .regular, design: .monospaced))
                        .foregroundStyle(selected ? SylvanderWorkspacePalette.text : SylvanderWorkspacePalette.dim)
                        .lineLimit(1)
                    HStack(spacing: 6) {
                        Text(session.workspaceName)
                            .lineLimit(1)
                        Text("·")
                        Text(relativeAge(session.lastSeenSeconds))
                    }
                    .font(.system(size: 9, weight: .regular, design: .monospaced))
                    .foregroundStyle(SylvanderWorkspacePalette.muted)
                }

                Spacer(minLength: 4)

                Text(session.presence.rawValue)
                    .font(.system(size: 8, weight: .semibold, design: .monospaced))
                    .foregroundStyle(presenceColor(session.presence))
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 13)
            .contentShape(Rectangle())
            .background(selected ? SylvanderWorkspacePalette.raised : SylvanderWorkspacePalette.panel)
            .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
            .overlay {
                RoundedRectangle(cornerRadius: 8, style: .continuous)
                    .stroke(selected ? SylvanderWorkspacePalette.active.opacity(0.85) : SylvanderWorkspacePalette.rule.opacity(0.65), lineWidth: 1)
            }
        }
        .buttonStyle(.plain)
        .padding(.horizontal, 10)
        .padding(.vertical, 5)
    }

    @ViewBuilder
    private func sessionSection(_ title: String, sessions: [SylvanderSession]) -> some View {
        if !sessions.isEmpty {
            Text(title)
                .font(.system(size: 9, weight: .bold, design: .monospaced))
                .tracking(1)
                .foregroundStyle(SylvanderWorkspacePalette.muted)
                .padding(.horizontal, 18)
                .padding(.top, title == "CONTINUE" ? 6 : 20)
                .padding(.bottom, 6)

            ForEach(sessions) { session in
                sessionRow(session)
            }
        }
    }

    private var continuingSessions: [SylvanderSession] {
        store.filteredSessions.filter { $0.presence == .active }
    }

    private var recentSessions: [SylvanderSession] {
        store.filteredSessions.filter { $0.presence != .active }
    }

    private func presenceColor(_ presence: SylvanderSession.Presence) -> Color {
        switch presence {
        case .active: SylvanderWorkspacePalette.active
        case .idle: SylvanderWorkspacePalette.idle
        case .away: SylvanderWorkspacePalette.muted
        }
    }

    private func relativeAge(_ seconds: UInt64) -> String {
        switch seconds {
        case 0..<60: "NOW"
        case 60..<3_600: "\(seconds / 60)M"
        case 3_600..<86_400: "\(seconds / 3_600)H"
        default: "\(seconds / 86_400)D"
        }
    }
}
#endif
