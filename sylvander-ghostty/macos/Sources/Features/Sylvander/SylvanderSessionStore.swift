#if os(macOS)
import Combine
import Foundation

@MainActor
final class SylvanderSessionStore: ObservableObject {
    static let selectedSessionDefaultsKey = "ai.oraculo.sylvander.workspace.selected-session"

    enum ConnectionState: Equatable {
        case connecting
        case online
        case offline(String)
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

    private let client: any SylvanderSessionFetching
    private let defaults: UserDefaults
    private var refreshTask: Task<Void, Never>?

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
            do {
                let sessions = try await client.fetchSessions()
                guard !Task.isCancelled else { return }
                self?.reconcile(sessions)
            } catch is CancellationError {
                return
            } catch {
                guard !Task.isCancelled else { return }
                self?.connectionState = .offline(error.localizedDescription)
            }
        }
    }

    func reconcile(_ incoming: [SylvanderSession]) {
        sessions = incoming.sorted {
            if $0.lastSeenSeconds != $1.lastSeenSeconds {
                return $0.lastSeenSeconds < $1.lastSeenSeconds
            }
            return $0.label.localizedCaseInsensitiveCompare($1.label) == .orderedAscending
        }

        if let selectedSessionID, sessions.contains(where: { $0.id == selectedSessionID }) {
            // Preserve the user's active terminal across server refreshes.
        } else {
            selectedSessionID = sessions.first?.id
        }
        connectionState = .online
    }
}
#endif
