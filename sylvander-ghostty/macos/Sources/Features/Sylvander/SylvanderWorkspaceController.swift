#if os(macOS)
import Cocoa
import Combine
import GhosttyKit
import SwiftUI

final class SylvanderWorkspaceController: BaseTerminalController {
    private static let frameAutosaveName = "SylvanderWorkspace"

    let sessionStore: SylvanderSessionStore
    @Published var preview: SylvanderPreview?
    @Published var changesWorkspace: String?
    @Published private(set) var launchFailure: String?

    private let socketPath: String
    private var sessionSurfaces: [String: Ghostty.SurfaceView] = [:]
    private var hostBroker: SylvanderHostBroker?
    private var cancellables: Set<AnyCancellable> = []

    init(
        _ ghostty: Ghostty.App,
        sessionStore: SylvanderSessionStore? = nil,
        socketPath: String = ProcessInfo.processInfo.environment["SYLVANDER_SOCKET"] ?? SylvanderSessionClient.defaultSocketPath
    ) {
        self.socketPath = socketPath
        self.preview = nil
        self.changesWorkspace = nil
        self.launchFailure = nil
        self.sessionStore = sessionStore ?? SylvanderSessionStore(
            client: SylvanderSessionClient(socketPath: socketPath)
        )
        super.init(ghostty, surfaceTree: .init())

        let broker = SylvanderHostBroker { [weak self] preview in
            self?.changesWorkspace = nil
            self?.preview = preview
        }
        do {
            try broker.start()
            hostBroker = broker
        } catch {
            hostBroker = nil
        }

        self.sessionStore.$selectedSessionID
            .removeDuplicates()
            .compactMap { $0 }
            .sink { [weak self] sessionID in
                self?.preview = nil
                self?.changesWorkspace = nil
                self?.activateSession(sessionID)
            }
            .store(in: &cancellables)
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) is not supported for this view")
    }

    override func loadWindow() {
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 1180, height: 760),
            styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
            backing: .buffered,
            defer: false
        )
        window.title = "Sylvander"
        window.titlebarAppearsTransparent = true
        window.titleVisibility = .hidden
        window.minSize = NSSize(width: 780, height: 520)
        window.isRestorable = false
        window.setFrameAutosaveName(Self.frameAutosaveName)
        window.backgroundColor = .black
        window.delegate = self
        window.contentView = NSHostingView(rootView: SylvanderWorkspaceView(controller: self))
        self.window = window
    }

    override func windowDidLoad() {
        super.windowDidLoad()
        if window?.setFrameUsingName(Self.frameAutosaveName) == false {
            window?.center()
        }
        sessionStore.refresh()
    }

    func loadWorkspaceWindowIfNeeded() {
        guard window == nil else { return }
        loadWindow()
        windowDidLoad()
    }

    func showChangesInspector() {
        guard let session = sessionStore.sessions.first(where: {
            $0.id == sessionStore.selectedSessionID
        }) else { return }
        preview = nil
        changesWorkspace = session.workspace
    }

    func activateSession(_ sessionID: String) {
        guard let session = sessionStore.sessions.first(where: { $0.id == sessionID }) else { return }

        let surface: Ghostty.SurfaceView
        if let existing = sessionSurfaces[sessionID] {
            surface = existing
        } else {
            guard let app = ghostty.app else { return }
            do {
                let credential = try hostBroker?.register(
                    sessionID: session.id,
                    workspace: session.workspace
                )
                let config = try SylvanderTUILaunchConfiguration(
                    session: session,
                    socketPath: socketPath,
                    hostCredential: credential
                ).surfaceConfiguration()
                surface = Ghostty.SurfaceView(app, baseConfig: config)
                sessionSurfaces[sessionID] = surface
            } catch {
                launchFailure = error.localizedDescription
                NSSound.beep()
                return
            }
        }

        launchFailure = nil
        setBackgroundSurfacesOccluded(except: surface)
        surfaceTree = SplitTree(view: surface)
        focusedSurface = surface
    }

    private func setBackgroundSurfacesOccluded(except visibleSurface: Ghostty.SurfaceView) {
        for surfaceView in sessionSurfaces.values where surfaceView !== visibleSurface {
            guard let surface = surfaceView.surface, surfaceView.isWindowVisible else { continue }
            ghostty_surface_set_occlusion(surface, false)
            surfaceView.isWindowVisible = false
            surfaceView.focusDidChange(false)
        }
    }
}

private struct SylvanderTUILaunchConfiguration {
    let session: SylvanderSession
    let socketPath: String
    let hostCredential: SylvanderHostBroker.SessionCredential?

    func surfaceConfiguration() throws -> Ghostty.SurfaceConfiguration {
        let executable = try executablePath()
        var config = Ghostty.SurfaceConfiguration()
        config.command = Ghostty.Shell.quote(executable)
        config.workingDirectory = session.workspace
        config.environmentVariables = [
            "SYLVANDER_DESKTOP_HOST": "ghostty",
            "SYLVANDER_SESSION": session.id,
            "SYLVANDER_SOCKET": socketPath,
            "SYLVANDER_WORKSPACE": session.workspace,
        ]
        if let hostCredential {
            config.environmentVariables["SYLVANDER_HOST_CAPABILITIES"] = "image_preview,web_preview"
            config.environmentVariables["SYLVANDER_HOST_SOCKET"] = hostCredential.socketPath
            config.environmentVariables["SYLVANDER_HOST_TOKEN"] = hostCredential.token
        }
        return config
    }

    private func executablePath() throws -> String {
        let environment = ProcessInfo.processInfo.environment
        let candidates = [
            environment["SYLVANDER_TUI_PATH"],
            Bundle.main.resourceURL?
                .appendingPathComponent("bin/sylvander-tui", isDirectory: false)
                .path,
        ].compactMap { $0 }

        guard let path = candidates.first(where: {
            FileManager.default.isExecutableFile(atPath: $0)
        }) else {
            throw LaunchError.executableNotFound
        }
        return path
    }

    enum LaunchError: LocalizedError {
        case executableNotFound

        var errorDescription: String? {
            "sylvander-tui is not bundled; set SYLVANDER_TUI_PATH for a development build"
        }
    }
}

private struct SylvanderWorkspaceView: View {
    @ObservedObject var controller: SylvanderWorkspaceController

    var body: some View {
        GeometryReader { geometry in
            ZStack(alignment: .trailing) {
                HStack(spacing: 0) {
                    SylvanderSessionSidebar(store: controller.sessionStore)
                    workspaceRule

                    VStack(spacing: 0) {
                        SylvanderSessionContextBar(
                            store: controller.sessionStore,
                            onShowChanges: controller.showChangesInspector
                        )
                        workspaceRule.frame(height: 1)

                        if controller.surfaceTree.isEmpty {
                            emptyState
                        } else {
                            TerminalView(
                                ghostty: controller.ghostty,
                                viewModel: controller,
                                delegate: controller
                            )
                        }
                    }
                }

                if geometry.size.width >= 1_060 {
                    inspector
                } else if hasInspector {
                    inspector
                        .shadow(color: Color.black.opacity(0.45), radius: 18, x: -8)
                        .transition(.move(edge: .trailing).combined(with: .opacity))
                        .zIndex(1)
                }
            }
            .animation(.easeOut(duration: 0.18), value: hasInspector)
        }
        .background(SylvanderWorkspacePalette.canvas)
        .ignoresSafeArea(.container, edges: .top)
    }

    @ViewBuilder
    private var inspector: some View {
        if let preview = controller.preview {
            SylvanderPreviewInspector(preview: preview) {
                controller.preview = nil
            }
        } else if let workspace = controller.changesWorkspace {
            SylvanderChangesInspector(workspace: workspace) {
                controller.changesWorkspace = nil
            }
        }
    }

    private var hasInspector: Bool {
        controller.preview != nil || controller.changesWorkspace != nil
    }

    private var workspaceRule: some View {
        Rectangle()
            .fill(SylvanderWorkspacePalette.rule)
            .frame(width: 1)
    }

    private var emptyState: some View {
        ZStack {
            SylvanderWorkspacePalette.canvas
            VStack(alignment: .leading, spacing: 12) {
                Text(emptyTitle)
                    .font(.system(size: 13, weight: .bold, design: .monospaced))
                    .tracking(1.4)
                    .foregroundStyle(SylvanderWorkspacePalette.warm)
                Text(emptyDetail)
                    .font(.system(size: 11, weight: .regular, design: .monospaced))
                    .foregroundStyle(SylvanderWorkspacePalette.dim)
                    .frame(maxWidth: 420, alignment: .leading)
            }
            .padding(36)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .bottomLeading)
        }
    }

    private var emptyTitle: String {
        if controller.launchFailure != nil { return "TERMINAL COULD NOT START" }
        return switch controller.sessionStore.connectionState {
        case .connecting: "DISCOVERING SESSIONS"
        case .online: "NO ACTIVE SESSION"
        case .offline: "SYLVANDER IS OFFLINE"
        }
    }

    private var emptyDetail: String {
        if let launchFailure = controller.launchFailure { return launchFailure }
        return switch controller.sessionStore.connectionState {
        case .connecting:
            "Negotiating the local desktop-host protocol."
        case .online:
            "Start a session from a channel or refresh after the server creates one."
        case .offline(let message):
            message
        }
    }
}
#endif
