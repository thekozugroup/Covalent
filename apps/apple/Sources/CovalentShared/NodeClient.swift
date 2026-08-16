import Foundation

public actor NodeClient {
    private let baseURL: URL
    private let session: URLSession
    private let decoder = JSONDecoder()

    public init(baseURL: URL = URL(string: "http://127.0.0.1:8787")!, session: URLSession? = nil) {
        self.baseURL = baseURL
        if let session {
            self.session = session
        } else {
            let configuration = URLSessionConfiguration.ephemeral
            configuration.requestCachePolicy = .reloadIgnoringLocalCacheData
            configuration.timeoutIntervalForRequest = 10
            self.session = URLSession(configuration: configuration)
        }
    }

    public func status() async throws -> NodeStatus {
        let url = baseURL.appending(path: "api/v1/status")
        var request = URLRequest(url: url)
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse, http.statusCode == 200 else {
            throw NodeClientError.invalidResponse
        }
        let status = try decoder.decode(NodeStatus.self, from: data)
        guard status.protocolVersion == covalentProtocolVersion else {
            throw NodeClientError.unsupportedProtocol(status.protocolVersion)
        }
        return status
    }
}

public enum NodeClientError: Error, Equatable {
    case invalidResponse
    case unsupportedProtocol(UInt16)
}
