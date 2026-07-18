#if os(macOS)
import Cocoa
import Combine
import GhosttyKit
import OSLog
import SwiftUI

final class SylvanderWorkspaceController: BaseTerminalController {
    private static let frameAutosaveName = "SylvanderWorkspace"
    private static let logger = Logger(
        subsystem: Bundle.main.bundleIdentifier ?? "ai.oraculo.sylvander",
        category: "desktop-sessions"
    )

    let sessionStore: SylvanderSessionStore
    @Published var preview: SylvanderPreview?
    @Published var changesWorkspace: String?
    @Published private(set) var launchFailure: String?

    private let socketPath: String
    private var sessionSurfaces: [String: Ghostty.SurfaceView] = [:]
    private var hostBroker: SylvanderHostBroker?
    private var cancellables: Set<AnyCancellable> = []
    private var launchRetryAttempts: [String: Int] = [:]
    private var launchRetryGeneration = 0

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

        Publishers.CombineLatest(
            self.sessionStore.$selectedSessionID,
            self.sessionStore.$sessions
        )
            .map { selectedSessionID, sessions -> SylvanderSession? in
                guard let selectedSessionID else { return nil }
                return sessions.first(where: { $0.id == selectedSessionID })
            }
            // Presence is refreshed every few seconds. It must not repeatedly
            // reset the active surface or serve as an accidental launch retry.
            .removeDuplicates { previous, current in
                previous?.id == current?.id &&
                    previous?.workspace == current?.workspace
            }
            .sink { [weak self] session in
                self?.cancelLaunchRetries()
                self?.preview = nil
                self?.changesWorkspace = nil
                if let session {
                    self?.activateSession(session)
                } else {
                    self?.deactivateSessions()
                }
            }
            .store(in: &cancellables)

        self.sessionStore.$sessions
            .map { Set($0.map(\.id)) }
            .removeDuplicates()
            .sink { [weak self] sessionIDs in
                self?.reclaimSessions(notIn: sessionIDs)
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
        SylvanderWorkspaceAppearance.apply(to: window)
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

    func activateSession(_ session: SylvanderSession) {
        let sessionID = session.id

        let surface: Ghostty.SurfaceView
        if let existing = sessionSurfaces[sessionID], !existing.processExited {
            surface = existing
        } else {
            sessionSurfaces[sessionID] = nil
            hostBroker?.unregister(sessionID: sessionID)
            guard let app = ghostty.app else {
                launchFailure = "Ghostty is not ready to create the session terminal"
                Self.logger.error("Ghostty runtime unavailable for session \(sessionID, privacy: .public)")
                return
            }
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
                if let error = surface.error {
                    throw error
                }
                sessionSurfaces[sessionID] = surface
                cancelLaunchRetries()
                Self.logger.info("Created terminal surface for session \(sessionID, privacy: .public)")
            } catch {
                launchFailure = error.localizedDescription
                Self.logger.error(
                    "Terminal launch failed for session \(sessionID, privacy: .public): \(error.localizedDescription, privacy: .public)"
                )
                if !scheduleLaunchRetry(for: session) {
                    NSSound.beep()
                }
                return
            }
        }

        launchFailure = nil
        setBackgroundSurfacesOccluded(except: surface)
        surfaceTree = SplitTree(view: surface)
        focusedSurface = surface
    }

    /// A surface can be requested during the first app tick even though
    /// `Ghostty.App` already reports ready. Retry that narrow startup race
    /// explicitly instead of relying on unrelated session-presence updates.
    @discardableResult
    private func scheduleLaunchRetry(for session: SylvanderSession) -> Bool {
        let attempt = (launchRetryAttempts[session.id] ?? 0) + 1
        let delays: [TimeInterval] = [0.15, 0.4, 0.9]
        guard attempt <= delays.count else {
            launchRetryAttempts[session.id] = nil
            return false
        }

        launchRetryAttempts[session.id] = attempt
        launchRetryGeneration &+= 1
        let generation = launchRetryGeneration
        Self.logger.notice(
            "Retrying terminal launch for session \(session.id, privacy: .public), attempt \(attempt)"
        )
        DispatchQueue.main.asyncAfter(deadline: .now() + delays[attempt - 1]) { [weak self] in
            guard let self,
                  self.launchRetryGeneration == generation,
                  self.sessionStore.selectedSessionID == session.id,
                  let current = self.sessionStore.sessions.first(where: { $0.id == session.id })
            else { return }
            self.activateSession(current)
        }
        return true
    }

    private func cancelLaunchRetries() {
        launchRetryGeneration &+= 1
        launchRetryAttempts.removeAll()
    }

    private func setBackgroundSurfacesOccluded(except visibleSurface: Ghostty.SurfaceView) {
        for surfaceView in sessionSurfaces.values where surfaceView !== visibleSurface {
            guard let surface = surfaceView.surface, surfaceView.isWindowVisible else { continue }
            ghostty_surface_set_occlusion(surface, false)
            surfaceView.isWindowVisible = false
            surfaceView.focusDidChange(false)
        }
    }

    private func reclaimSessions(notIn validSessionIDs: Set<String>) {
        let removedSessionIDs = Set(sessionSurfaces.keys).subtracting(validSessionIDs)
        for sessionID in removedSessionIDs {
            if let surface = sessionSurfaces.removeValue(forKey: sessionID) {
                surface.isWindowVisible = false
                surface.focusDidChange(false)
            }
            hostBroker?.unregister(sessionID: sessionID)
        }
        if let selectedSessionID = sessionStore.selectedSessionID,
           !validSessionIDs.contains(selectedSessionID) {
            deactivateSessions()
        }
    }

    private func deactivateSessions() {
        focusedSurface = nil
        surfaceTree = .init()
    }
}

struct SylvanderTUILaunchConfiguration {
    let session: SylvanderSession
    let socketPath: String
    let hostCredential: SylvanderHostBroker.SessionCredential?

    func surfaceConfiguration() throws -> Ghostty.SurfaceConfiguration {
        let executable = try executablePath()
        var config = Ghostty.SurfaceConfiguration()
        config.command = Ghostty.Shell.quote(executable)
        config.workingDirectory = session.workspace
        config.environmentVariables = environmentVariables()
        return config
    }

    /// Environment owned by the desktop host for every embedded TUI.
    ///
    /// Ghostty also publishes `TERM` and `COLORTERM` while constructing the
    /// child process. Keeping them explicit here makes the Sylvander launch
    /// contract deterministic even when a future surface path changes how the
    /// inherited environment is assembled.
    func environmentVariables() -> [String: String] {
        var environment = [
            "CLICOLOR": "1",
            "CLICOLOR_FORCE": "1",
            "COLORTERM": "truecolor",
            "SYLVANDER_DESKTOP_HOST": "ghostty",
            "SYLVANDER_SESSION": session.id,
            "SYLVANDER_SOCKET": socketPath,
            "SYLVANDER_TUI_COLOR": "truecolor",
            "SYLVANDER_WORKSPACE": session.workspace,
            "TERM": "xterm-ghostty",
        ]
        if let hostCredential {
            environment["SYLVANDER_HOST_CAPABILITIES"] = "image_preview,web_preview"
            environment["SYLVANDER_HOST_SOCKET"] = hostCredential.socketPath
            environment["SYLVANDER_HOST_TOKEN"] = hostCredential.token
        }
        return environment
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

/// The desktop owns the capabilities of its embedded terminal surface.
///
/// `NO_COLOR` is intentionally removed from the process instead of being
/// assigned an empty value: clients commonly treat the mere presence of that
/// variable as a monochrome request. Ghostty itself publishes the final
/// `TERM=xterm-ghostty` and `COLORTERM=truecolor` values inside the PTY.
enum SylvanderTUILaunchEnvironment {
    static func prepareProcessEnvironment(
        unset: (String) -> Void = { unsetenv($0) }
    ) {
        unset("NO_COLOR")
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
        .background {
            SylvanderDesktopMaterial()
                .overlay(SylvanderWorkspacePalette.canvas)
        }
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
        case .recovering: "RECONNECTING TO SYLVANDER"
        }
    }

    private var emptyDetail: String {
        if let launchFailure = controller.launchFailure { return launchFailure }
        return switch controller.sessionStore.connectionState {
        case .connecting:
            "Negotiating the local desktop-host protocol."
        case .online:
            "Start a session from a channel or refresh after the server creates one."
        case .recovering(let message, let attempt):
            "Attempt \(attempt) · \(message)"
        }
    }
}
#endif
