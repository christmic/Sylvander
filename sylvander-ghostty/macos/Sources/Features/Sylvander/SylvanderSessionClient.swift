#if os(macOS)
import Foundation
import Network

protocol SylvanderSessionFetching: Sendable {
    func fetchSessions() async throws -> [SylvanderSession]
    func fetchAgents() async throws -> [SylvanderAgent]
    func createSession(label: String, agentID: String, workspace: String?) async throws -> String
    func renameSession(id: String, label: String) async throws
    func archiveSession(id: String) async throws
    func deleteSession(id: String) async throws
    func activityEvents(for id: String) -> AsyncThrowingStream<SylvanderSessionActivity, Error>
}

extension SylvanderSessionFetching {
    func fetchAgents() async throws -> [SylvanderAgent] { throw SylvanderSessionClient.ClientError.unsupportedOperation }
    func createSession(label: String, agentID: String, workspace: String?) async throws -> String {
        throw SylvanderSessionClient.ClientError.unsupportedOperation
    }
    func renameSession(id: String, label: String) async throws { throw SylvanderSessionClient.ClientError.unsupportedOperation }
    func archiveSession(id: String) async throws { throw SylvanderSessionClient.ClientError.unsupportedOperation }
    func deleteSession(id: String) async throws { throw SylvanderSessionClient.ClientError.unsupportedOperation }
    func activityEvents(for id: String) -> AsyncThrowingStream<SylvanderSessionActivity, Error> {
        AsyncThrowingStream { $0.finish() }
    }
}

struct SylvanderSessionClient: SylvanderSessionFetching {
    static let defaultSocketPath = "/tmp/sylvander.sock"
    static let maximumLineBytes = 1_048_576
    /// The desktop host deliberately supports only the repository's current
    /// UI contract. Pre-release protocol revisions are not negotiated.
    static let protocolVersion = 4

    let socketPath: String

    init(socketPath: String = ProcessInfo.processInfo.environment["SYLVANDER_SOCKET"] ?? defaultSocketPath) {
        self.socketPath = socketPath
    }

    func fetchSessions() async throws -> [SylvanderSession] {
        try await request(
            ["type": "list_sessions"],
            decode: Self.decodeSessions
        )
    }

    func fetchAgents() async throws -> [SylvanderAgent] {
        try await request(
            ["type": "discover_agents"],
            decode: Self.decodeAgents
        )
    }

    func createSession(label: String, agentID: String, workspace: String?) async throws -> String {
        var overrides: [String: Any] = [:]
        if let workspace {
            overrides["user_workspace"] = [
                "execution_target": "local",
                "path": workspace,
                "read_only": false,
            ]
        }
        return try await request([
            "type": "create_session",
            "request": [
                "agent_id": agentID,
                "label": label,
                "overrides": overrides,
            ],
        ]) { data in
            let envelope = try Self.decodeAction(data, expectedType: "session_created")
            guard let sessionID = envelope.sessionID else { throw ClientError.unexpectedMessage(envelope.type) }
            return sessionID
        }
    }

    func renameSession(id: String, label: String) async throws {
        try await request([
            "type": "rename_session",
            "session_id": id,
            "label": label,
        ]) { data in
            _ = try Self.decodeAction(data, expectedType: "session_updated")
        }
    }

    func archiveSession(id: String) async throws {
        try await request([
            "type": "archive_session",
            "session_id": id,
        ]) { data in
            _ = try Self.decodeAction(data, expectedType: "session_updated")
        }
    }

    func deleteSession(id: String) async throws {
        try await request([
            "type": "delete_session",
            "session_id": id,
        ]) { data in
            _ = try Self.decodeAction(data, expectedType: "session_deleted")
        }
    }

    func activityEvents(for id: String) -> AsyncThrowingStream<SylvanderSessionActivity, Error> {
        AsyncThrowingStream { continuation in
            let task = Task {
                let connection = NWConnection(to: .unix(path: socketPath), using: .tcp)
                let reader = SylvanderLineReader(
                    connection: connection,
                    maximumBytes: Self.maximumLineBytes
                )
                defer { connection.cancel() }
                do {
                    try await connection.startAndWait()
                    try await connection.sendLine(Self.helloLine)
                    try Self.validateHandshake(try await reader.receiveLine())
                    let message = try JSONSerialization.data(withJSONObject: [
                        "type": "load_session",
                        "session_id": id,
                    ], options: [.sortedKeys])
                    guard let line = String(data: message, encoding: .utf8) else {
                        throw ClientError.invalidRequest
                    }
                    try await connection.sendLine(line)
                    while !Task.isCancelled {
                        let data = try await reader.receiveLine()
                        if let activity = Self.decodeActivity(data) {
                            continuation.yield(activity)
                        }
                    }
                    continuation.finish()
                } catch is CancellationError {
                    continuation.finish()
                } catch {
                    continuation.finish(throwing: error)
                }
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }

    private func request<T>(
        _ message: [String: Any],
        decode: (Data) throws -> T
    ) async throws -> T {
        let connection = NWConnection(to: .unix(path: socketPath), using: .tcp)
        let reader = SylvanderLineReader(
            connection: connection,
            maximumBytes: Self.maximumLineBytes
        )
        let timeout = DispatchWorkItem { connection.cancel() }
        DispatchQueue.global(qos: .userInitiated).asyncAfter(deadline: .now() + 2, execute: timeout)
        defer {
            timeout.cancel()
            connection.cancel()
        }

        try await connection.startAndWait()
        try await connection.sendLine(Self.helloLine)

        let handshake = try await reader.receiveLine()
        try Self.validateHandshake(handshake)

        let requestData = try JSONSerialization.data(withJSONObject: message, options: [.sortedKeys])
        guard let requestLine = String(data: requestData, encoding: .utf8) else {
            throw ClientError.invalidRequest
        }
        try await connection.sendLine(requestLine)
        let response = try await reader.receiveLine()
        return try decode(response)
    }

    static func validateHandshake(_ data: Data) throws {
        let envelope = try JSONDecoder().decode(HandshakeEnvelope.self, from: data)
        switch envelope.type {
        case "welcome":
            guard envelope.protocolInfo?.version == protocolVersion else {
                throw ClientError.unsupportedProtocol(envelope.protocolInfo?.version)
            }
        case "protocol_error":
            throw ClientError.protocolRejected(envelope.error?.message ?? "unknown protocol error")
        default:
            throw ClientError.unexpectedMessage(envelope.type)
        }
    }

    static func decodeSessions(_ data: Data) throws -> [SylvanderSession] {
        let envelope = try JSONDecoder().decode(SessionEnvelope.self, from: data)
        guard envelope.type == "sessions_list" else {
            throw ClientError.unexpectedMessage(envelope.type)
        }
        return envelope.sessions
    }

    static func decodeAgents(_ data: Data) throws -> [SylvanderAgent] {
        let envelope = try JSONDecoder().decode(AgentEnvelope.self, from: data)
        guard envelope.type == "agents_discovered" else {
            if let message = envelope.message { throw ClientError.operationFailed(message) }
            throw ClientError.unexpectedMessage(envelope.type)
        }
        return envelope.agents ?? []
    }

    static func decodeActivity(_ data: Data) -> SylvanderSessionActivity? {
        guard let envelope = try? JSONDecoder().decode(ActivityEnvelope.self, from: data) else { return nil }
        switch envelope.type {
        case "iteration_start", "text_delta", "thinking_delta", "tool_call", "tool_output_delta",
             "model_retry", "task_started", "task_progress":
            return .running
        case "approval_request", "ask_user", "plan_proposed", "interaction_timeout":
            return .waiting
        case "done", "task_completed":
            return .complete
        case "error", "task_failed", "compaction_failed", "workspace_rollback_failed":
            return .failed
        case "tool_result" where envelope.isError == true:
            return .failed
        case "turn_interrupted", "task_cancelled":
            return .idle
        default:
            return nil
        }
    }

    private static func decodeAction(_ data: Data, expectedType: String) throws -> ActionEnvelope {
        let envelope = try JSONDecoder().decode(ActionEnvelope.self, from: data)
        if envelope.type == "operation_error" {
            throw ClientError.operationFailed(envelope.message ?? "operation failed")
        }
        guard envelope.type == expectedType else { throw ClientError.unexpectedMessage(envelope.type) }
        return envelope
    }

    static let helloLine = #"{"type":"hello","protocol":{"client_name":"sylvander-ghostty","min_version":\#(protocolVersion),"max_version":\#(protocolVersion),"capabilities":["desktop_host","sessions"]}}"#

    enum ClientError: LocalizedError, Equatable {
        case connection(String)
        case closed
        case lineTooLong
        case invalidRequest
        case operationFailed(String)
        case protocolRejected(String)
        case unsupportedOperation
        case unexpectedMessage(String)
        case unsupportedProtocol(Int?)

        var errorDescription: String? {
            switch self {
            case .connection(let message): "Unable to reach Sylvander: \(message)"
            case .closed: "Sylvander closed the session connection"
            case .lineTooLong: "Sylvander sent an oversized protocol message"
            case .invalidRequest: "Unable to encode the Sylvander request"
            case .operationFailed(let message): message
            case .protocolRejected(let message): "Protocol rejected: \(message)"
            case .unsupportedOperation: "This Sylvander connection does not support session management"
            case .unexpectedMessage(let type): "Unexpected server message: \(type)"
            case .unsupportedProtocol(let version): "Unsupported protocol version: \(version.map(String.init) ?? "missing")"
            }
        }
    }

    private struct HandshakeEnvelope: Decodable {
        let type: String
        let protocolInfo: ProtocolInfo?
        let error: ProtocolError?

        enum CodingKeys: String, CodingKey {
            case type
            case protocolInfo = "protocol"
            case error
        }
    }

    private struct ProtocolInfo: Decodable {
        let version: Int
    }

    private struct ProtocolError: Decodable {
        let message: String
    }

    private struct SessionEnvelope: Decodable {
        let type: String
        let sessions: [SylvanderSession]
    }

    private struct AgentEnvelope: Decodable {
        let type: String
        let agents: [SylvanderAgent]?
        let message: String?
    }

    private struct ActionEnvelope: Decodable {
        let type: String
        let sessionID: String?
        let message: String?

        enum CodingKeys: String, CodingKey {
            case type
            case sessionID = "session_id"
            case message
        }
    }

    private struct ActivityEnvelope: Decodable {
        let type: String
        let isError: Bool?

        enum CodingKeys: String, CodingKey {
            case type
            case isError = "is_error"
        }
    }
}

private extension NWConnection {
    func startAndWait() async throws {
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            let gate = ContinuationGate(continuation)
            stateUpdateHandler = { state in
                switch state {
                case .ready:
                    gate.resume()
                case .failed(let error):
                    gate.resume(throwing: SylvanderSessionClient.ClientError.connection(error.localizedDescription))
                case .cancelled:
                    gate.resume(throwing: SylvanderSessionClient.ClientError.connection("connection timed out"))
                default:
                    break
                }
            }
            start(queue: DispatchQueue(label: "com.sylvander.session-client"))
        }
    }

    func sendLine(_ line: String) async throws {
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            send(content: Data((line + "\n").utf8), completion: .contentProcessed { error in
                if let error {
                    continuation.resume(throwing: SylvanderSessionClient.ClientError.connection(error.localizedDescription))
                } else {
                    continuation.resume()
                }
            })
        }
    }

    func receiveChunk(maximumBytes: Int) async throws -> Data {
        try await withCheckedThrowingContinuation { continuation in
            receive(minimumIncompleteLength: 1, maximumLength: maximumBytes) { data, _, isComplete, error in
                if let error {
                    continuation.resume(throwing: SylvanderSessionClient.ClientError.connection(error.localizedDescription))
                } else if let data, !data.isEmpty {
                    continuation.resume(returning: data)
                } else if isComplete {
                    continuation.resume(throwing: SylvanderSessionClient.ClientError.closed)
                } else {
                    continuation.resume(throwing: SylvanderSessionClient.ClientError.closed)
                }
            }
        }
    }
}

/// Stateful NDJSON reader that preserves every byte following the first
/// newline. `NWConnection.receive` may coalesce several server events in one
/// callback, so a stateless `receiveLine` would silently discard activity.
private final class SylvanderLineReader: @unchecked Sendable {
    private let connection: NWConnection
    private let maximumBytes: Int
    private var buffer: SylvanderLineBuffer

    init(connection: NWConnection, maximumBytes: Int) {
        self.connection = connection
        self.maximumBytes = maximumBytes
        self.buffer = SylvanderLineBuffer(maximumBytes: maximumBytes)
    }

    func receiveLine() async throws -> Data {
        while true {
            if let line = try buffer.popLine() {
                return line
            }
            let room = max(1, min(4096, maximumBytes - buffer.byteCount + 1))
            let chunk = try await connection.receiveChunk(maximumBytes: room)
            try buffer.append(chunk)
        }
    }
}

struct SylvanderLineBuffer {
    let maximumBytes: Int
    private var storage = Data()

    init(maximumBytes: Int) {
        self.maximumBytes = maximumBytes
    }

    var byteCount: Int { storage.count }

    mutating func append(_ data: Data) throws {
        storage.append(data)
        if storage.firstIndex(of: 0x0A) == nil, storage.count > maximumBytes {
            throw SylvanderSessionClient.ClientError.lineTooLong
        }
    }

    mutating func popLine() throws -> Data? {
        guard let newline = storage.firstIndex(of: 0x0A) else { return nil }
        guard newline <= maximumBytes else {
            throw SylvanderSessionClient.ClientError.lineTooLong
        }
        let line = Data(storage[..<newline])
        storage.removeSubrange(...newline)
        return line
    }
}

private final class ContinuationGate: @unchecked Sendable {
    private let lock = NSLock()
    private var continuation: CheckedContinuation<Void, Error>?

    init(_ continuation: CheckedContinuation<Void, Error>) {
        self.continuation = continuation
    }

    func resume() {
        take()?.resume()
    }

    func resume(throwing error: Error) {
        take()?.resume(throwing: error)
    }

    private func take() -> CheckedContinuation<Void, Error>? {
        lock.lock()
        defer { lock.unlock() }
        let value = continuation
        continuation = nil
        return value
    }
}
#endif
