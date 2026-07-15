#if os(macOS)
import Foundation
import Network

protocol SylvanderSessionFetching: Sendable {
    func fetchSessions() async throws -> [SylvanderSession]
}

struct SylvanderSessionClient: SylvanderSessionFetching {
    static let defaultSocketPath = "/tmp/sylvander.sock"
    static let maximumLineBytes = 1_048_576

    let socketPath: String

    init(socketPath: String = ProcessInfo.processInfo.environment["SYLVANDER_SOCKET"] ?? defaultSocketPath) {
        self.socketPath = socketPath
    }

    func fetchSessions() async throws -> [SylvanderSession] {
        let connection = NWConnection(to: .unix(path: socketPath), using: .tcp)
        let timeout = DispatchWorkItem { connection.cancel() }
        DispatchQueue.global(qos: .userInitiated).asyncAfter(deadline: .now() + 2, execute: timeout)
        defer {
            timeout.cancel()
            connection.cancel()
        }

        try await connection.startAndWait()
        try await connection.sendLine(Self.helloLine)

        let handshake = try await connection.receiveLine(maximumBytes: Self.maximumLineBytes)
        try Self.validateHandshake(handshake)

        try await connection.sendLine(#"{"type":"list_sessions"}"#)
        let response = try await connection.receiveLine(maximumBytes: Self.maximumLineBytes)
        return try Self.decodeSessions(response)
    }

    static func validateHandshake(_ data: Data) throws {
        let envelope = try JSONDecoder().decode(HandshakeEnvelope.self, from: data)
        switch envelope.type {
        case "welcome":
            guard let version = envelope.protocolInfo?.version, (1...2).contains(version) else {
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

    private static let helloLine = #"{"type":"hello","protocol":{"client_name":"sylvander-ghostty","min_version":1,"max_version":2,"capabilities":["desktop_host","sessions"]}}"#

    enum ClientError: LocalizedError, Equatable {
        case connection(String)
        case closed
        case lineTooLong
        case protocolRejected(String)
        case unexpectedMessage(String)
        case unsupportedProtocol(Int?)

        var errorDescription: String? {
            switch self {
            case .connection(let message): "Unable to reach Sylvander: \(message)"
            case .closed: "Sylvander closed the session connection"
            case .lineTooLong: "Sylvander sent an oversized protocol message"
            case .protocolRejected(let message): "Protocol rejected: \(message)"
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

    func receiveLine(maximumBytes: Int) async throws -> Data {
        var buffer = Data()
        while buffer.count <= maximumBytes {
            let chunk = try await receiveChunk(maximumBytes: min(4096, maximumBytes - buffer.count + 1))
            buffer.append(chunk)
            if let newline = buffer.firstIndex(of: 0x0A) {
                return buffer[..<newline]
            }
        }
        throw SylvanderSessionClient.ClientError.lineTooLong
    }

    private func receiveChunk(maximumBytes: Int) async throws -> Data {
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
