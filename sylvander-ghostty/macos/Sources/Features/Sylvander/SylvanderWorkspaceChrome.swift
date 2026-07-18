#if os(macOS)
import AppKit
import SwiftUI

enum SylvanderWorkspacePalette {
    static let canvas = Color(red: 0.024, green: 0.028, blue: 0.035).opacity(0.54)
    static let panel = Color(red: 0.043, green: 0.050, blue: 0.061).opacity(0.72)
    static let raised = Color(red: 0.063, green: 0.073, blue: 0.088).opacity(0.82)
    static let text = Color(red: 0.925, green: 0.906, blue: 0.871)
    static let dim = Color(red: 0.596, green: 0.608, blue: 0.616)
    static let muted = Color(red: 0.400, green: 0.424, blue: 0.447)
    static let rule = Color(red: 0.204, green: 0.227, blue: 0.251)
    static let warm = Color(red: 0.941, green: 0.745, blue: 0.447)
    static let active = Color(red: 0.459, green: 0.655, blue: 0.910)
    static let idle = Color(red: 0.851, green: 0.686, blue: 0.384)
    static let signal = Color(red: 0.878, green: 0.424, blue: 0.459)
    static let complete = Color(red: 0.345, green: 0.722, blue: 0.655)
}

/// AppKit appearance policy shared by the workspace window and its SwiftUI
/// material bridge.
///
/// Keeping these values in one place prevents a future window refactor from
/// silently making the terminal opaque while leaving the decorative material
/// in the view hierarchy.
@MainActor
enum SylvanderWorkspaceAppearance {
    static func apply(to window: NSWindow) {
        window.isOpaque = false
        window.backgroundColor = .clear
    }

    static func makeDesktopMaterialView() -> NSVisualEffectView {
        let view = NSVisualEffectView()
        view.material = .underWindowBackground
        view.blendingMode = .behindWindow
        view.state = .active
        view.isEmphasized = true
        return view
    }
}

/// Native desktop material visible beneath Sylvander's translucent chrome.
///
/// Keeping this behind the SwiftUI palette preserves the dark visual system
/// while allowing wallpaper and neighbouring windows to remain perceptible.
struct SylvanderDesktopMaterial: NSViewRepresentable {
    func makeNSView(context: Context) -> NSVisualEffectView {
        SylvanderWorkspaceAppearance.makeDesktopMaterialView()
    }

    func updateNSView(_ view: NSVisualEffectView, context: Context) {
        view.state = .active
    }
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
