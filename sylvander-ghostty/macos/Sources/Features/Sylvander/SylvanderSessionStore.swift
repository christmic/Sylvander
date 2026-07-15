#if os(macOS)
import Combine
import Foundation

@MainActor
final class SylvanderSessionStore: ObservableObject {
    static let selectedSessionDefaultsKey = "ai.oraculo.sylvander.workspace.selected-session"
    static let maximumActivityMonitors = 32

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
            if let selectedSessionID {
                unreadSessionIDs.remove(selectedSessionID)
            }
            reconcileActivityMonitors()
        }
    }
    @Published var query = ""
    @Published private(set) var connectionState: ConnectionState = .connecting
    @Published private(set) var operationState: OperationState = .idle
    @Published private(set) var operationError: String?
    @Published private(set) var activities: [String: SylvanderSessionActivity] = [:]
    @Published private(set) var unreadSessionIDs: Set<String> = []

    private let client: any SylvanderSessionFetching
    private let defaults: UserDefaults
    private var refreshTask: Task<Void, Never>?
    private var pendingSelectionID: String?
    private var activityTasks: [String: Task<Void, Never>] = [:]
    private var activityStartedAt: [String: ContinuousClock.Instant] = [:]

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
        activityTasks.values.forEach { $0.cancel() }
    }

    var filteredSessions: [SylvanderSession] {
        let term = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !term.isEmpty else { return sessions }
        return sessions.filter {
            $0.label.localizedCaseInsensitiveContains(term) ||
                $0.workspace.localizedCaseInsensitiveContains(term)
        }
    }

    func activity(for sessionID: String) -> SylvanderSessionActivity {
        activities[sessionID] ?? .idle
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
        reconcileActivityMonitors()
    }

    private func reconcileActivityMonitors() {
        var validIDs = Set(sessions.prefix(Self.maximumActivityMonitors).map(\.id))
        if let selectedSessionID { validIDs.insert(selectedSessionID) }
        for sessionID in Set(activityTasks.keys).subtracting(validIDs) {
            activityTasks.removeValue(forKey: sessionID)?.cancel()
            activities[sessionID] = nil
            unreadSessionIDs.remove(sessionID)
            activityStartedAt[sessionID] = nil
        }
        for sessionID in validIDs where activityTasks[sessionID] == nil {
            activityStartedAt[sessionID] = .now
            activityTasks[sessionID] = Task { [weak self, client] in
                var retryDelay = 1
                while !Task.isCancelled {
                    do {
                        for try await activity in client.activityEvents(for: sessionID) {
                            guard !Task.isCancelled else { return }
                            self?.apply(activity, to: sessionID)
                            retryDelay = 1
                        }
                    } catch is CancellationError {
                        return
                    } catch {
                        // Discovery owns the visible connection error; monitors reconnect quietly.
                    }
                    try? await Task.sleep(for: .seconds(retryDelay))
                    retryDelay = min(retryDelay * 2, 15)
                }
            }
        }
    }

    func apply(_ activity: SylvanderSessionActivity, to sessionID: String) {
        let changed = activities[sessionID] != activity
        activities[sessionID] = activity
        guard changed, sessionID != selectedSessionID else { return }
        let hasFinishedPriming = activityStartedAt[sessionID].map {
            $0.duration(to: .now) >= .seconds(1)
        } ?? true
        if hasFinishedPriming {
            unreadSessionIDs.insert(sessionID)
        }
    }
}
#endif
