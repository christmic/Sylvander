#if os(macOS)
import Combine
import Foundation

@MainActor
final class SylvanderSessionStore: ObservableObject {
    static let selectedSessionDefaultsKey = "ai.oraculo.sylvander.workspace.selected-session"

    enum ConnectionState: Equatable {
        case connecting
        case online
        case recovering(message: String, attempt: Int)
    }

    enum OperationState: Equatable {
        case idle
        case creating
        case mutating(sessionID: String)
    }

    @Published private(set) var sessions: [SylvanderSession] = []
    @Published var selectedSessionID: String? {
        didSet {
            guard selectedSessionID != oldValue else { return }
            if let selectedSessionID {
                defaults.set(selectedSessionID, forKey: Self.selectedSessionDefaultsKey)
            } else {
                defaults.removeObject(forKey: Self.selectedSessionDefaultsKey)
            }
        }
    }
    @Published var query = ""
    @Published private(set) var connectionState: ConnectionState = .connecting
    @Published private(set) var operationState: OperationState = .idle
    @Published private(set) var operationError: String?

    private let client: any SylvanderSessionFetching
    private let defaults: UserDefaults
    private var refreshTask: Task<Void, Never>?
    private var pendingSelectionID: String?

    init(
        client: any SylvanderSessionFetching = SylvanderSessionClient(),
        defaults: UserDefaults = .ghostty
    ) {
        self.client = client
        self.defaults = defaults
        self.selectedSessionID = defaults.string(forKey: Self.selectedSessionDefaultsKey)
    }

    deinit {
        refreshTask?.cancel()
    }

    var filteredSessions: [SylvanderSession] {
        let term = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !term.isEmpty else { return sessions }
        return sessions.filter {
            $0.label.localizedCaseInsensitiveContains(term) ||
                $0.workspace.localizedCaseInsensitiveContains(term)
        }
    }

    func refresh() {
        refreshTask?.cancel()
        connectionState = .connecting
        refreshTask = Task { [weak self, client] in
            var attempt = 0
            while !Task.isCancelled {
                do {
                    let sessions = try await client.fetchSessions()
                    guard !Task.isCancelled else { return }
                    self?.reconcile(sessions)
                    attempt = 0
                    try await Task.sleep(for: .seconds(5))
                } catch is CancellationError {
                    return
                } catch {
                    guard !Task.isCancelled else { return }
                    attempt += 1
                    self?.connectionState = .recovering(
                        message: error.localizedDescription,
                        attempt: attempt
                    )
                    let delay = min(30, 1 << min(attempt, 5))
                    try? await Task.sleep(for: .seconds(delay))
                }
            }
        }
    }

    func fetchAgents() async -> [SylvanderAgent] {
        operationError = nil
        do {
            return try await client.fetchAgents()
        } catch {
            operationError = error.localizedDescription
            return []
        }
    }

    @discardableResult
    func createSession(label: String, agentID: String, workspace: String?) async -> Bool {
        let cleanLabel = label.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !cleanLabel.isEmpty else {
            operationError = "Give the session a name before creating it."
            return false
        }
        operationState = .creating
        operationError = nil
        defer { operationState = .idle }
        do {
            pendingSelectionID = try await client.createSession(
                label: cleanLabel,
                agentID: agentID,
                workspace: workspace
            )
            refresh()
            return true
        } catch {
            operationError = error.localizedDescription
            return false
        }
    }

    @discardableResult
    func renameSession(id: String, label: String) async -> Bool {
        let cleanLabel = label.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !cleanLabel.isEmpty else {
            operationError = "A session name cannot be empty."
            return false
        }
        return await mutate(id: id) { client in
            try await client.renameSession(id: id, label: cleanLabel)
        }
    }

    @discardableResult
    func archiveSession(id: String) async -> Bool {
        await mutate(id: id) { client in try await client.archiveSession(id: id) }
    }

    @discardableResult
    func deleteSession(id: String) async -> Bool {
        await mutate(id: id) { client in try await client.deleteSession(id: id) }
    }

    func clearOperationError() {
        operationError = nil
    }

    private func mutate(
        id: String,
        operation: (any SylvanderSessionFetching) async throws -> Void
    ) async -> Bool {
        operationState = .mutating(sessionID: id)
        operationError = nil
        defer { operationState = .idle }
        do {
            try await operation(client)
            refresh()
            return true
        } catch {
            operationError = error.localizedDescription
            return false
        }
    }

    func reconcile(_ incoming: [SylvanderSession]) {
        sessions = incoming.sorted {
            if $0.lastSeenSeconds != $1.lastSeenSeconds {
                return $0.lastSeenSeconds < $1.lastSeenSeconds
            }
            return $0.label.localizedCaseInsensitiveCompare($1.label) == .orderedAscending
        }

        if let pendingSelectionID, sessions.contains(where: { $0.id == pendingSelectionID }) {
            selectedSessionID = pendingSelectionID
            self.pendingSelectionID = nil
        } else if let selectedSessionID, sessions.contains(where: { $0.id == selectedSessionID }) {
            // Preserve the user's active terminal across server refreshes.
        } else {
            selectedSessionID = sessions.first?.id
        }
        connectionState = .online
    }
}
#endif
