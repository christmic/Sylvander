import AppKit
import Foundation
import Network
import Testing
@testable import Ghostty

struct SylvanderSessionTests {
    @Test
    func desktopLaunchEnvironmentForcesTrueColorAndRemovesNoColor() {
        var unsetNames: [String] = []
        SylvanderTUILaunchEnvironment.prepareProcessEnvironment(
            unset: { unsetNames.append($0) }
        )

        #expect(unsetNames == ["NO_COLOR"])
    }

    @Test @MainActor
    func workspaceAppearanceKeepsWindowAndMaterialTranslucent() {
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 640, height: 480),
            styleMask: [.titled],
            backing: .buffered,
            defer: false
        )

        SylvanderWorkspaceAppearance.apply(to: window)
        let material = SylvanderWorkspaceAppearance.makeDesktopMaterialView()

        #expect(window.isOpaque == false)
        #expect(window.backgroundColor.isEqual(NSColor.clear))
        #expect(material.material == .underWindowBackground)
        #expect(material.blendingMode == .behindWindow)
        #expect(material.state == .active)
        #expect(material.isEmphasized)
    }

    @Test
    func decodesSessionList() throws {
        let data = Data(#"{"type":"sessions_list","sessions":[{"id":"s-1","label":"Audit auth","workspace":"/work/api","last_seen_secs":7}]}"#.utf8)

        let sessions = try SylvanderSessionClient.decodeSessions(data)

        #expect(sessions == [
            SylvanderSession(
                id: "s-1",
                label: "Audit auth",
                workspace: "/work/api",
                lastSeenSeconds: 7
            ),
        ])
        #expect(sessions[0].presence == .active)
        #expect(sessions[0].workspaceName == "api")
    }

    @Test
    func rejectsUnexpectedSessionResponse() {
        let data = Data(#"{"type":"done","sessions":[]}"#.utf8)

        #expect(throws: SylvanderSessionClient.ClientError.unexpectedMessage("done")) {
            try SylvanderSessionClient.decodeSessions(data)
        }
    }

    @Test
    func validatesNegotiatedProtocolRange() throws {
        try SylvanderSessionClient.validateHandshake(
            Data(#"{"type":"welcome","protocol":{"server_name":"sylvander","version":2,"capabilities":[]}}"#.utf8)
        )

        #expect(throws: SylvanderSessionClient.ClientError.unsupportedProtocol(3)) {
            try SylvanderSessionClient.validateHandshake(
                Data(#"{"type":"welcome","protocol":{"server_name":"sylvander","version":3,"capabilities":[]}}"#.utf8)
            )
        }
    }

    @Test
    func decodesDiscoveredAgentsAndWorkspace() throws {
        let data = Data(#"{"type":"agents_discovered","agents":[{"id":"code","name":"Code","agent_workspace":{"execution_target":"local","path":"/work/code","read_only":false}}]}"#.utf8)

        let agents = try SylvanderSessionClient.decodeAgents(data)

        #expect(agents.map(\.id) == ["code"])
        #expect(agents.first?.name == "Code")
        #expect(agents.first?.agentWorkspace?.path == "/work/code")
    }

    @Test
    func classifiesSemanticSessionActivity() {
        #expect(SylvanderSessionClient.decodeActivity(Data(#"{"type":"iteration_start"}"#.utf8)) == .running)
        #expect(SylvanderSessionClient.decodeActivity(Data(#"{"type":"approval_request"}"#.utf8)) == .waiting)
        #expect(SylvanderSessionClient.decodeActivity(Data(#"{"type":"done"}"#.utf8)) == .complete)
        #expect(SylvanderSessionClient.decodeActivity(Data(#"{"type":"tool_result","is_error":true}"#.utf8)) == .failed)
        #expect(SylvanderSessionClient.decodeActivity(Data(#"{"type":"session_history"}"#.utf8)) == nil)
    }

    @Test
    func lineBufferPreservesCoalescedAndPartialProtocolFrames() throws {
        var buffer = SylvanderLineBuffer(maximumBytes: 32)
        try buffer.append(Data("first\nsecond\npar".utf8))

        let firstLine = try buffer.popLine()
        let secondLine = try buffer.popLine()
        let incomplete = try buffer.popLine()
        let first = try #require(firstLine)
        let second = try #require(secondLine)
        #expect(String(decoding: first, as: UTF8.self) == "first")
        #expect(String(decoding: second, as: UTF8.self) == "second")
        #expect(incomplete == nil)

        try buffer.append(Data("tial\n".utf8))
        let partialLine = try buffer.popLine()
        let partial = try #require(partialLine)
        #expect(String(decoding: partial, as: UTF8.self) == "partial")
    }

    @Test
    func lineBufferRejectsAnOversizedFrameBeforeNewline() throws {
        var buffer = SylvanderLineBuffer(maximumBytes: 4)
        #expect(throws: SylvanderSessionClient.ClientError.lineTooLong) {
            try buffer.append(Data("12345".utf8))
        }
    }

    @Test @MainActor
    func reconciliationKeepsSelectionAndSortsByActivity() {
        let suite = "SylvanderSessionTests.\(#function)"
        let defaults = UserDefaults(suiteName: suite)!
        defer { defaults.removePersistentDomain(forName: suite) }
        defaults.removePersistentDomain(forName: suite)
        let store = SylvanderSessionStore(client: StubSessionClient(), defaults: defaults)
        let older = SylvanderSession(
            id: "older",
            label: "Older",
            workspace: "/work/older",
            lastSeenSeconds: 90
        )
        let active = SylvanderSession(
            id: "active",
            label: "Active",
            workspace: "/work/active",
            lastSeenSeconds: 2
        )

        store.reconcile([older, active])
        store.selectedSessionID = "older"
        store.reconcile([active, older])

        #expect(store.sessions.map(\.id) == ["active", "older"])
        #expect(store.selectedSessionID == "older")
        #expect(store.connectionState == .online)
    }

    @Test @MainActor
    func restoresSelectionAndFallsBackWhenSessionDisappears() throws {
        let suite = "SylvanderSessionTests.\(#function)"
        let defaults = UserDefaults(suiteName: suite)!
        defer { defaults.removePersistentDomain(forName: suite) }
        defaults.removePersistentDomain(forName: suite)
        defaults.set("restored", forKey: SylvanderSessionStore.selectedSessionDefaultsKey)
        let store = SylvanderSessionStore(client: StubSessionClient(), defaults: defaults)
        let restored = SylvanderSession(
            id: "restored", label: "Restored", workspace: "/work/restored", lastSeenSeconds: 5
        )
        let replacement = SylvanderSession(
            id: "replacement", label: "Replacement", workspace: "/work/new", lastSeenSeconds: 1
        )

        store.reconcile([replacement, restored])
        #expect(store.selectedSessionID == "restored")

        store.reconcile([replacement])
        #expect(store.selectedSessionID == "replacement")
        #expect(defaults.string(forKey: SylvanderSessionStore.selectedSessionDefaultsKey) == "replacement")
    }

    @Test @MainActor
    func managementOperationsRefreshAndSelectCreatedSession() async {
        let suite = "SylvanderSessionTests.\(#function)"
        let defaults = UserDefaults(suiteName: suite)!
        defer { defaults.removePersistentDomain(forName: suite) }
        let client = ManagingSessionClient()
        let store = SylvanderSessionStore(client: client, defaults: defaults)

        #expect(await store.createSession(label: "  New work  ", agentID: "code", workspace: "/work/new"))
        store.reconcile([
            SylvanderSession(id: "created", label: "New work", workspace: "/work/new", lastSeenSeconds: 0),
        ])
        #expect(store.selectedSessionID == "created")
        #expect(client.createdLabel == "New work")

        #expect(await store.renameSession(id: "created", label: "Renamed"))
        #expect(await store.archiveSession(id: "created"))
        #expect(await store.deleteSession(id: "created"))
        #expect(client.operations == ["rename:created:Renamed", "archive:created", "delete:created"])
    }

    @Test @MainActor
    func selectedSessionActivityDoesNotBecomeUnread() {
        let suite = "SylvanderSessionTests.\(#function)"
        let defaults = UserDefaults(suiteName: suite)!
        defer { defaults.removePersistentDomain(forName: suite) }
        let store = SylvanderSessionStore(client: StubSessionClient(), defaults: defaults)
        store.reconcile([
            SylvanderSession(id: "focused", label: "Focused", workspace: "/work", lastSeenSeconds: 0),
        ])

        store.apply(.waiting, to: "focused")

        #expect(store.activity(for: "focused") == .waiting)
        #expect(store.unreadSessionIDs.isEmpty)
    }

}

struct SylvanderPreviewPolicyTests {
    @Test(arguments: [
        "http://example.com",
        "https://user:secret@example.com",
        "file:///tmp/index.html",
        "javascript:alert(1)",
    ])
    func rejectsUnsafeWebTargets(_ target: String) {
        #expect(throws: SylvanderPreviewPolicy.PolicyError.unsafeWebURL) {
            try SylvanderPreviewPolicy.webURL(target: target)
        }
    }

    @Test
    func acceptsPublicHTTPSWebTarget() throws {
        let url = try SylvanderPreviewPolicy.webURL(target: "https://example.com:8443/docs?q=host")
        #expect(url.absoluteString == "https://example.com:8443/docs?q=host")
    }

    @Test(arguments: [
        "http://localhost:3000/settings",
        "http://127.0.0.1:5173",
        "http://[::1]:8080",
        "http://10.0.0.4:3000",
        "https://dev.local:8443/docs",
    ])
    func acceptsLocalDevelopmentWebTarget(_ target: String) throws {
        #expect(try SylvanderPreviewPolicy.webURL(target: target).absoluteString == target)
    }

    @Test
    func rejectsImageOutsideWorkspaceAndSymlinkEscape() throws {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        let workspace = root.appendingPathComponent("workspace", isDirectory: true)
        let outside = root.appendingPathComponent("outside", isDirectory: true)
        try FileManager.default.createDirectory(at: workspace, withIntermediateDirectories: true)
        try FileManager.default.createDirectory(at: outside, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let outsideImage = outside.appendingPathComponent("secret.png")
        try Data([0]).write(to: outsideImage)
        let link = workspace.appendingPathComponent("linked.png")
        try FileManager.default.createSymbolicLink(at: link, withDestinationURL: outsideImage)

        #expect(throws: SylvanderPreviewPolicy.PolicyError.imageOutsideWorkspace) {
            try SylvanderPreviewPolicy.imageURL(
                target: "../outside/secret.png",
                workspace: workspace.path
            )
        }
        #expect(throws: SylvanderPreviewPolicy.PolicyError.imageOutsideWorkspace) {
            try SylvanderPreviewPolicy.imageURL(target: link.path, workspace: workspace.path)
        }
    }
}

struct SylvanderChangesSnapshotTests {
    @Test
    func includesStagedAndBoundedUntrackedFilesWithoutHead() throws {
        let workspace = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: workspace, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: workspace) }
        try runGit(["init", "--quiet"], at: workspace)
        try Data("staged content\n".utf8).write(to: workspace.appendingPathComponent("staged.txt"))
        try runGit(["add", "staged.txt"], at: workspace)
        try Data("untracked content\n".utf8).write(to: workspace.appendingPathComponent("notes.txt"))

        let snapshot = ChangesSnapshot.load(workspace: workspace.path)

        #expect(snapshot.files == ["notes.txt", "staged.txt"])
        #expect(snapshot.diff.contains("+staged content"))
        #expect(snapshot.diff.contains("new file (untracked)"))
        #expect(snapshot.diff.contains("+untracked content"))
        #expect(snapshot.message == nil)
    }

    private func runGit(_ arguments: [String], at workspace: URL) throws {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/git")
        process.arguments = ["-C", workspace.path] + arguments
        try process.run()
        process.waitUntilExit()
        #expect(process.terminationStatus == 0)
    }
}

struct SylvanderHostBrokerTests {
    @Test @MainActor
    func acceptsOnlyRegisteredSessionCapability() async throws {
        var captured: SylvanderPreview?
        let broker = SylvanderHostBroker { preview in captured = preview }
        try broker.start()
        defer { broker.stop() }
        let credential = try broker.register(sessionID: "session-a", workspace: "/tmp")

        let accepted = try await send(
            socketPath: credential.socketPath,
            request: [
                "version": 1,
                "session_id": "session-a",
                "token": credential.token,
                "kind": "web",
                "target": "https://example.com/docs",
            ]
        )
        await Task.yield()

        #expect(accepted["ok"] as? Bool == true)
        #expect(captured == .web(URL(string: "https://example.com/docs")!))

        let rejected = try await send(
            socketPath: credential.socketPath,
            request: [
                "version": 1,
                "session_id": "session-a",
                "token": "wrong-token",
                "kind": "web",
                "target": "https://example.com/private",
            ]
        )
        #expect(rejected["ok"] as? Bool == false)
        #expect(captured == .web(URL(string: "https://example.com/docs")!))
    }

    private func send(socketPath: String, request: [String: Any]) async throws -> [String: Any] {
        let connection = NWConnection(to: .unix(path: socketPath), using: .tcp)
        defer { connection.cancel() }
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            connection.stateUpdateHandler = { state in
                switch state {
                case .ready:
                    connection.stateUpdateHandler = nil
                    continuation.resume()
                case .failed(let error):
                    connection.stateUpdateHandler = nil
                    continuation.resume(throwing: error)
                default:
                    break
                }
            }
            connection.start(queue: DispatchQueue(label: "ai.oraculo.sylvander.host-broker-tests"))
        }

        var data = try JSONSerialization.data(withJSONObject: request)
        data.append(0x0A)
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            connection.send(content: data, completion: .contentProcessed { error in
                if let error {
                    continuation.resume(throwing: error)
                } else {
                    continuation.resume()
                }
            })
        }
        let response = try await withCheckedThrowingContinuation {
            (continuation: CheckedContinuation<Data, Error>) in
            connection.receive(minimumIncompleteLength: 1, maximumLength: 16 * 1024) {
                data, _, _, error in
                if let error {
                    continuation.resume(throwing: error)
                } else {
                    continuation.resume(returning: data ?? Data())
                }
            }
        }
        return try #require(JSONSerialization.jsonObject(with: response) as? [String: Any])
    }
}

private struct StubSessionClient: SylvanderSessionFetching {
    func fetchSessions() async throws -> [SylvanderSession] { [] }
}

private final class ManagingSessionClient: SylvanderSessionFetching, @unchecked Sendable {
    var createdLabel: String?
    var operations: [String] = []

    func fetchSessions() async throws -> [SylvanderSession] { [] }
    func fetchAgents() async throws -> [SylvanderAgent] { [] }

    func createSession(label: String, agentID: String, workspace: String?) async throws -> String {
        createdLabel = label
        return "created"
    }

    func renameSession(id: String, label: String) async throws {
        operations.append("rename:\(id):\(label)")
    }

    func archiveSession(id: String) async throws { operations.append("archive:\(id)") }
    func deleteSession(id: String) async throws { operations.append("delete:\(id)") }
}
