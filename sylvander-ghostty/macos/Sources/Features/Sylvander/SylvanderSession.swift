#if os(macOS)
import Foundation

struct SylvanderSession: Codable, Hashable, Identifiable, Sendable {
    let id: String
    let label: String
    let workspace: String
    let lastSeenSeconds: UInt64

    enum CodingKeys: String, CodingKey {
        case id
        case label
        case workspace
        case lastSeenSeconds = "last_seen_secs"
    }

    var presence: Presence {
        switch lastSeenSeconds {
        case 0..<15: .active
        case 15..<300: .idle
        default: .away
        }
    }

    var workspaceName: String {
        let name = URL(fileURLWithPath: workspace).lastPathComponent
        return name.isEmpty ? workspace : name
    }

    enum Presence: String, Sendable {
        case active = "ACTIVE"
        case idle = "IDLE"
        case away = "AWAY"
    }
}

struct SylvanderAgent: Decodable, Hashable, Identifiable, Sendable {
    let id: String
    let name: String
    let agentWorkspace: Workspace?

    enum CodingKeys: String, CodingKey {
        case id
        case name
        case agentWorkspace = "agent_workspace"
    }

    struct Workspace: Decodable, Hashable, Sendable {
        let path: String
    }
}
#endif
