#if os(macOS)
import SwiftUI

enum SylvanderWorkspacePalette {
    static let canvas = Color(red: 0.024, green: 0.028, blue: 0.035)
    static let panel = Color(red: 0.043, green: 0.050, blue: 0.061)
    static let raised = Color(red: 0.063, green: 0.073, blue: 0.088)
    static let text = Color(red: 0.925, green: 0.906, blue: 0.871)
    static let dim = Color(red: 0.596, green: 0.608, blue: 0.616)
    static let muted = Color(red: 0.400, green: 0.424, blue: 0.447)
    static let rule = Color(red: 0.204, green: 0.227, blue: 0.251)
    static let warm = Color(red: 0.941, green: 0.745, blue: 0.447)
    static let active = Color(red: 0.459, green: 0.655, blue: 0.910)
    static let idle = Color(red: 0.851, green: 0.686, blue: 0.384)
    static let signal = Color(red: 0.878, green: 0.424, blue: 0.459)
}

struct SylvanderSessionContextBar: View {
    @ObservedObject var store: SylvanderSessionStore
    let onShowChanges: () -> Void

    var body: some View {
        HStack(spacing: 12) {
            VStack(alignment: .leading, spacing: 4) {
                Text(selectedSession?.label ?? "SESSION WORKBENCH")
                    .font(.system(size: 13, weight: .semibold, design: .monospaced))
                    .foregroundStyle(SylvanderWorkspacePalette.text)
                    .lineLimit(1)
                Text(selectedSession?.workspace ?? "Choose a session to continue working")
                    .font(.system(size: 9, weight: .medium, design: .monospaced))
                    .foregroundStyle(SylvanderWorkspacePalette.muted)
                    .lineLimit(1)
            }

            Spacer(minLength: 12)

            Button(action: onShowChanges) {
                Label("REVIEW CHANGES", systemImage: "arrow.triangle.branch")
            }
            .buttonStyle(.plain)
            .font(.system(size: 9, weight: .bold, design: .monospaced))
            .tracking(0.5)
            .foregroundStyle(SylvanderWorkspacePalette.active)
            .disabled(selectedSession == nil)
            .opacity(selectedSession == nil ? 0.45 : 1)
            .help("Review uncommitted workspace changes")
        }
        .padding(.horizontal, 24)
        .frame(height: 58)
        .background(SylvanderWorkspacePalette.panel)
    }

    private var selectedSession: SylvanderSession? {
        store.sessions.first { $0.id == store.selectedSessionID }
    }

}

#endif
