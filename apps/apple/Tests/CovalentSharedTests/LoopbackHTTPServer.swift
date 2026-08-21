import Foundation
import Network

/// A minimal HTTP/1.1 server bound to loopback, used to drive `NodeClient`
/// through the **real** CFNetwork stack.
///
/// Every other test in this target injects a `URLSession` whose
/// `protocolClasses` is a `URLProtocol` stub, which never touches CFNetwork at
/// all. That is how an `NSInputStream` class-cluster crash in the archive
/// upload body reached CI green: the stub read `httpBodyStream` directly, so
/// nothing ever handed the stream to URLSession. This server closes that gap.
final class LoopbackHTTPServer: @unchecked Sendable {
    struct Request: Sendable {
        let method: String
        let path: String
        let headers: [String: String]
        let body: Data

        func header(_ name: String) -> String? { headers[name.lowercased()] }
    }

    struct Response: Sendable {
        var status: Int = 200
        var headers: [String: String] = [:]
        var body: Data = Data()

        static func json(_ text: String, status: Int = 200, headers: [String: String] = [:]) -> Response {
            var merged = headers
            merged["Content-Type"] = "application/json"
            return Response(status: status, headers: merged, body: Data(text.utf8))
        }
    }

    private let listener: NWListener
    private let queue = DispatchQueue(label: "covalent.loopback.http")
    private let handler: @Sendable (Request) -> Response
    private let lock = NSLock()
    private var recorded: [Request] = []

    /// Requests the server has served, in order.
    var requests: [Request] {
        lock.lock()
        defer { lock.unlock() }
        return recorded
    }

    init(handler: @escaping @Sendable (Request) -> Response) throws {
        self.handler = handler
        let parameters = NWParameters.tcp
        parameters.requiredInterfaceType = .loopback
        parameters.allowLocalEndpointReuse = true
        listener = try NWListener(using: parameters, on: .any)
    }

    /// Starts listening and returns the bound loopback port.
    func start() async throws -> UInt16 {
        listener.newConnectionHandler = { [weak self] connection in
            self?.accept(connection)
        }
        return try await withCheckedThrowingContinuation { continuation in
            let resumed = ResumeGuard()
            listener.stateUpdateHandler = { [weak self] state in
                switch state {
                case .ready:
                    guard let port = self?.listener.port?.rawValue, resumed.claim() else { return }
                    continuation.resume(returning: port)
                case let .failed(error):
                    if resumed.claim() { continuation.resume(throwing: error) }
                default:
                    break
                }
            }
            listener.start(queue: queue)
        }
    }

    func stop() {
        listener.cancel()
    }

    private func accept(_ connection: NWConnection) {
        connection.start(queue: queue)
        receiveRequest(on: connection, buffer: Data())
    }

    private func receiveRequest(on connection: NWConnection, buffer: Data) {
        connection.receive(minimumIncompleteLength: 1, maximumLength: 64 * 1_024) { [weak self] chunk, _, isComplete, error in
            guard let self else { return }
            guard error == nil else {
                connection.cancel()
                return
            }
            var accumulated = buffer
            if let chunk { accumulated.append(chunk) }

            guard let headerEnd = Self.range(of: Data("\r\n\r\n".utf8), in: accumulated) else {
                if isComplete {
                    connection.cancel()
                } else {
                    self.receiveRequest(on: connection, buffer: accumulated)
                }
                return
            }
            let headerData = accumulated.prefix(upTo: headerEnd.lowerBound)
            guard let parsed = Self.parseHead(headerData) else {
                connection.cancel()
                return
            }
            let expectedLength = Int(parsed.headers["content-length"] ?? "0") ?? 0
            let bodySoFar = accumulated.suffix(from: headerEnd.upperBound)
            guard bodySoFar.count >= expectedLength else {
                if isComplete {
                    connection.cancel()
                } else {
                    self.receiveRequest(on: connection, buffer: accumulated)
                }
                return
            }
            let body = Data(bodySoFar.prefix(expectedLength))
            let request = Request(
                method: parsed.method,
                path: parsed.path,
                headers: parsed.headers,
                body: body
            )
            self.lock.lock()
            self.recorded.append(request)
            self.lock.unlock()
            self.send(self.handler(request), on: connection)
        }
    }

    private func send(_ response: Response, on connection: NWConnection) {
        var head = "HTTP/1.1 \(response.status) \(Self.reason(response.status))\r\n"
        var headers = response.headers
        headers["Content-Length"] = String(response.body.count)
        headers["Connection"] = "close"
        for (name, value) in headers.sorted(by: { $0.key < $1.key }) {
            head += "\(name): \(value)\r\n"
        }
        head += "\r\n"
        var payload = Data(head.utf8)
        payload.append(response.body)
        connection.send(
            content: payload,
            completion: .contentProcessed { _ in connection.cancel() }
        )
    }

    private static func parseHead(_ data: Data) -> (method: String, path: String, headers: [String: String])? {
        guard let text = String(data: data, encoding: .utf8) else { return nil }
        var lines = text.components(separatedBy: "\r\n")
        guard !lines.isEmpty else { return nil }
        let requestLine = lines.removeFirst().split(separator: " ")
        guard requestLine.count >= 2 else { return nil }
        var headers: [String: String] = [:]
        for line in lines where line.contains(":") {
            guard let separator = line.firstIndex(of: ":") else { continue }
            let name = line[line.startIndex..<separator].trimmingCharacters(in: .whitespaces).lowercased()
            let value = line[line.index(after: separator)...].trimmingCharacters(in: .whitespaces)
            headers[name] = value
        }
        return (String(requestLine[0]), String(requestLine[1]), headers)
    }

    private static func range(of needle: Data, in haystack: Data) -> Range<Data.Index>? {
        haystack.range(of: needle)
    }

    private static func reason(_ status: Int) -> String {
        switch status {
        case 200: "OK"
        case 204: "No Content"
        case 401: "Unauthorized"
        case 409: "Conflict"
        case 507: "Insufficient Storage"
        default: "Status"
        }
    }
}

/// Guarantees a `CheckedContinuation` is resumed exactly once even though
/// `stateUpdateHandler` can fire repeatedly.
private final class ResumeGuard: @unchecked Sendable {
    private let lock = NSLock()
    private var used = false

    func claim() -> Bool {
        lock.lock()
        defer { lock.unlock() }
        guard !used else { return false }
        used = true
        return true
    }
}
