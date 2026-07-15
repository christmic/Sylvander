#if os(macOS)
import Darwin
import Foundation
import Security

final class SylvanderHostBroker: @unchecked Sendable {
    enum PreviewKind: String, Codable, Sendable {
        case image
        case web
    }

    struct PreviewRequest: Sendable {
        let sessionID: String
        let kind: PreviewKind
        let target: String
        let workspace: String
    }

    struct SessionCredential: Sendable {
        let socketPath: String
        let token: String
    }

    enum BrokerError: LocalizedError {
        case socketPathTooLong
        case systemCall(String, Int32)
        case randomToken

        var errorDescription: String? {
            switch self {
            case .socketPathTooLong: "Host broker socket path is too long"
            case .systemCall(let call, let code): "Host broker \(call) failed (errno \(code))"
            case .randomToken: "Host broker could not create a secure session token"
            }
        }
    }

    private struct Registration {
        let token: String
        let workspace: String
    }

    private struct WireRequest: Decodable {
        let version: Int
        let sessionID: String
        let token: String
        let kind: PreviewKind
        let target: String

        enum CodingKeys: String, CodingKey {
            case version
            case sessionID = "session_id"
            case token
            case kind
            case target
        }
    }

    private struct WireResponse: Encodable {
        let ok: Bool
        let message: String
    }

    private static let maximumFrameBytes = 64 * 1024
    private let queue = DispatchQueue(label: "ai.oraculo.sylvander.host-broker", qos: .userInitiated)
    private let socketPath: String
    private let onPreview: @MainActor @Sendable (SylvanderPreview) -> Void
    private var registrations: [String: Registration] = [:]
    private var listener: DispatchSourceRead?
    private var listenerFD: Int32 = -1

    init(onPreview: @escaping @MainActor @Sendable (SylvanderPreview) -> Void) {
        socketPath = URL(fileURLWithPath: "/tmp", isDirectory: true)
            .appendingPathComponent("sylvander-host-\(getpid())-\(UUID().uuidString).sock")
            .path
        self.onPreview = onPreview
    }

    deinit {
        stop()
    }

    func start() throws {
        try queue.sync {
            guard listenerFD < 0 else { return }
            let fd = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
            guard fd >= 0 else { throw BrokerError.systemCall("socket", errno) }
            do {
                guard fcntl(fd, F_SETFL, fcntl(fd, F_GETFL) | O_NONBLOCK) == 0 else {
                    throw BrokerError.systemCall("fcntl", errno)
                }
                try bindSocket(fd)
                guard Darwin.chmod(socketPath, S_IRUSR | S_IWUSR) == 0 else {
                    throw BrokerError.systemCall("chmod", errno)
                }
                guard Darwin.listen(fd, 16) == 0 else {
                    throw BrokerError.systemCall("listen", errno)
                }
            } catch {
                Darwin.close(fd)
                unlink(socketPath)
                throw error
            }

            listenerFD = fd
            let source = DispatchSource.makeReadSource(fileDescriptor: fd, queue: queue)
            source.setEventHandler { [weak self] in self?.acceptPendingConnections() }
            source.setCancelHandler { Darwin.close(fd) }
            listener = source
            source.resume()
        }
    }

    func stop() {
        queue.sync {
            listener?.cancel()
            listener = nil
            listenerFD = -1
            registrations.removeAll()
            unlink(socketPath)
        }
    }

    func register(sessionID: String, workspace: String) throws -> SessionCredential {
        let token = try Self.secureToken()
        queue.sync {
            registrations[sessionID] = Registration(token: token, workspace: workspace)
        }
        return SessionCredential(socketPath: socketPath, token: token)
    }

    func unregister(sessionID: String) {
        queue.sync { registrations[sessionID] = nil }
    }

    private func bindSocket(_ fd: Int32) throws {
        var address = sockaddr_un()
        address.sun_family = sa_family_t(AF_UNIX)
        let path = Array(socketPath.utf8CString)
        let capacity = MemoryLayout.size(ofValue: address.sun_path)
        guard path.count <= capacity else { throw BrokerError.socketPathTooLong }
        withUnsafeMutableBytes(of: &address.sun_path) { bytes in
            for (index, byte) in path.enumerated() {
                bytes[index] = UInt8(bitPattern: byte)
            }
        }
        let length = socklen_t(MemoryLayout<sa_family_t>.size + path.count)
        let result = withUnsafePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                Darwin.bind(fd, $0, length)
            }
        }
        guard result == 0 else { throw BrokerError.systemCall("bind", errno) }
    }

    private func acceptPendingConnections() {
        while true {
            let client = Darwin.accept(listenerFD, nil, nil)
            if client < 0 {
                if errno == EAGAIN || errno == EWOULDBLOCK { return }
                return
            }
            _ = fcntl(client, F_SETFL, fcntl(client, F_GETFL) & ~O_NONBLOCK)
            var timeout = timeval(tv_sec: 2, tv_usec: 0)
            setsockopt(client, SOL_SOCKET, SO_RCVTIMEO, &timeout, socklen_t(MemoryLayout.size(ofValue: timeout)))
            handle(client: client)
        }
    }

    private func handle(client: Int32) {
        defer { Darwin.close(client) }
        do {
            let data = try readFrame(client)
            let wire = try JSONDecoder().decode(WireRequest.self, from: data)
            guard wire.version == 1 else {
                return writeResponse(client, ok: false, message: "unsupported host protocol")
            }
            guard let registration = registrations[wire.sessionID],
                  Self.constantTimeEqual(registration.token, wire.token) else {
                return writeResponse(client, ok: false, message: "invalid session capability")
            }
            guard !wire.target.isEmpty, wire.target.utf8.count <= 8 * 1024 else {
                return writeResponse(client, ok: false, message: "invalid preview target")
            }

            let request = PreviewRequest(
                sessionID: wire.sessionID,
                kind: wire.kind,
                target: wire.target,
                workspace: registration.workspace
            )
            let preview = try SylvanderPreviewPolicy.resolve(request)
            DispatchQueue.main.async { [onPreview] in onPreview(preview) }
            writeResponse(client, ok: true, message: "Opened host preview")
        } catch {
            writeResponse(client, ok: false, message: error.localizedDescription)
        }
    }

    private func readFrame(_ fd: Int32) throws -> Data {
        var data = Data()
        var buffer = [UInt8](repeating: 0, count: 4096)
        while data.count <= Self.maximumFrameBytes {
            let count = Darwin.recv(fd, &buffer, buffer.count, 0)
            guard count > 0 else { throw BrokerError.systemCall("recv", errno) }
            data.append(buffer, count: count)
            if let newline = data.firstIndex(of: 0x0A) {
                return data[..<newline]
            }
        }
        throw BrokerError.systemCall("frame", EMSGSIZE)
    }

    private func writeResponse(_ fd: Int32, ok: Bool, message: String) {
        guard var data = try? JSONEncoder().encode(WireResponse(ok: ok, message: message)) else { return }
        data.append(0x0A)
        data.withUnsafeBytes { bytes in
            _ = Darwin.send(fd, bytes.baseAddress, bytes.count, 0)
        }
    }

    private static func secureToken() throws -> String {
        var bytes = [UInt8](repeating: 0, count: 32)
        guard SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes) == errSecSuccess else {
            throw BrokerError.randomToken
        }
        return bytes.map { String(format: "%02x", $0) }.joined()
    }

    private static func constantTimeEqual(_ lhs: String, _ rhs: String) -> Bool {
        let left = Array(lhs.utf8)
        let right = Array(rhs.utf8)
        guard left.count == right.count else { return false }
        return zip(left, right).reduce(UInt8(0)) { $0 | ($1.0 ^ $1.1) } == 0
    }
}
#endif
