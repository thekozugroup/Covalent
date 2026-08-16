import Foundation

public struct NodeConnectionConfiguration: Equatable, Sendable {
    public let baseURL: URL
    public let apiToken: String?

    public init(baseURL: URL, apiToken: String?) throws {
        guard let scheme = baseURL.scheme?.lowercased(), ["http", "https"].contains(scheme),
              baseURL.host != nil,
              baseURL.user == nil,
              baseURL.password == nil,
              baseURL.query == nil,
              baseURL.fragment == nil
        else {
            throw NodeClientError.invalidServiceURL
        }
        let token = apiToken?.trimmingCharacters(in: .whitespacesAndNewlines)
        if let token, !token.isEmpty, !(32...512).contains(token.utf8.count) {
            throw NodeClientError.invalidToken
        }
        self.baseURL = baseURL
        self.apiToken = token?.isEmpty == false ? token : nil
    }

    public static var localDefault: Self {
        try! Self(baseURL: URL(string: "http://127.0.0.1:8787")!, apiToken: nil)
    }
}

public actor NodeClient {
    private let configuration: NodeConnectionConfiguration
    private let session: URLSession
    private let decoder: JSONDecoder
    private let encoder: JSONEncoder

    public init(
        configuration: NodeConnectionConfiguration = .localDefault,
        session: URLSession? = nil
    ) {
        self.configuration = configuration
        if let session {
            self.session = session
        } else {
            let sessionConfiguration = URLSessionConfiguration.ephemeral
            sessionConfiguration.requestCachePolicy = .reloadIgnoringLocalCacheData
            sessionConfiguration.timeoutIntervalForRequest = 20
            sessionConfiguration.timeoutIntervalForResource = 3_600
            sessionConfiguration.urlCache = nil
            self.session = URLSession(configuration: sessionConfiguration)
        }
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        self.decoder = decoder
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        encoder.dateEncodingStrategy = .iso8601
        self.encoder = encoder
    }

    public func status() async throws -> NodeStatus {
        let status: NodeStatus = try await send(path: "api/v1/status", authenticated: false)
        guard status.protocolVersion == covalentProtocolVersion else {
            throw NodeClientError.unsupportedProtocol(status.protocolVersion)
        }
        return status
    }

    public func transportIdentity() async throws -> TransportIdentity {
        try await send(path: "api/v1/transport/identity")
    }

    public func discoveryCandidates() async throws -> [DiscoveryCandidate] {
        try await send(path: "api/v1/discovery")
    }

    public func exportSettings() async throws -> ExportedDeviceSettings {
        try await send(path: "api/v1/config/export", method: "POST")
    }

    public func importSettings(_ settings: ExportedDeviceSettings, confirmed: Bool) async throws {
        let body = ConfigImportRequest(confirmed: confirmed, settings: settings)
        try await sendNoContent(path: "api/v1/config/import", body: body)
    }

    public func createPairingInvitation(lifetimeMilliseconds: UInt64, endpoints: [String]) async throws -> PairingInvitation {
        let body = PairInvitationRequest(lifetimeMs: lifetimeMilliseconds, endpoints: endpoints)
        return try await send(path: "api/v1/pair/invitations", method: "POST", body: body)
    }

    public func acceptPairingInvitation(
        _ invitation: PairingInvitation,
        responderName: String,
        responderRoles: Set<PeerRole>,
        inviterRoles: Set<PeerRole>
    ) async throws -> PairingSession {
        let body = PairAcceptRequest(
            invitation: invitation,
            responderName: responderName,
            responderRoles: responderRoles,
            inviterRoles: inviterRoles
        )
        return try await send(path: "api/v1/pair/accept", method: "POST", body: body)
    }

    public func confirmPairingAsResponder(_ session: PairingSession, displayedCode: String) async throws -> PairingSession {
        try await send(
            path: "api/v1/pair/confirm/responder",
            method: "POST",
            body: PairConfirmRequest(session: session, displayedCode: displayedCode)
        )
    }

    public func confirmPairingAsInviter(_ session: PairingSession, displayedCode: String) async throws -> PairingSession {
        try await send(
            path: "api/v1/pair/confirm/inviter",
            method: "POST",
            body: PairConfirmRequest(session: session, displayedCode: displayedCode)
        )
    }

    public func finalizePairingAsResponder(_ session: PairingSession) async throws -> PairingConfirmation {
        try await send(
            path: "api/v1/pair/finalize/responder",
            method: "POST",
            body: PairFinalizeRequest(session: session)
        )
    }

    public func finalizePairingAsInviter(_ session: PairingSession) async throws -> PairingConfirmation {
        try await send(
            path: "api/v1/pair/finalize/inviter",
            method: "POST",
            body: PairFinalizeRequest(session: session)
        )
    }

    public func providers() async throws -> [ProviderConnection] {
        try await send(path: "api/v1/providers")
    }

    public func backups() async throws -> [BackupSummary] {
        try await send(path: "api/v1/backups")
    }

    public func connectProvider(peerId: UUID, address: String, certificateDer: String) async throws -> ProviderConnection {
        try await send(
            path: "api/v1/providers/connect",
            method: "POST",
            body: ConnectProviderRequest(peerId: peerId, address: address, certificateDer: certificateDer)
        )
    }

    public func disconnectProvider(peerId: UUID) async throws {
        try await sendNoContent(
            path: "api/v1/providers/disconnect",
            body: PeerRequest(peerId: peerId)
        )
    }

    public func revokePeer(peerId: UUID) async throws {
        try await sendNoContent(path: "api/v1/peers/revoke", body: PeerRequest(peerId: peerId))
    }

    public func createBackup(_ request: BackupRequest) async throws -> BackupResponse {
        try await send(path: "api/v1/backups", method: "POST", body: request, timeout: 86_400)
    }

    public func createBackupArchive(sourceURL: URL, metadata: ArchiveBackupMetadata) async throws -> BackupResponse {
        guard metadata.protocolVersion == covalentProtocolVersion else {
            throw NodeClientError.unsupportedProtocol(metadata.protocolVersion)
        }
        let archiveURL = try await Task.detached(priority: .userInitiated) {
            try AppleArchiveTransfer.makeBackupArchive(sourceURL: sourceURL)
        }.value
        defer { try? FileManager.default.removeItem(at: archiveURL) }
        let metadataData = try encoder.encode(metadata)
        guard metadataData.count <= 32 * 1_024 else {
            throw NodeClientError.invalidPayload("Archive metadata exceeds 32 KiB.")
        }
        var request = try authenticatedRequest(
            path: "api/v1/backups/archive",
            method: "POST",
            accept: "application/json"
        )
        request.timeoutInterval = 86_400
        request.setValue(AppleArchiveTransfer.backupContentType, forHTTPHeaderField: "Content-Type")
        request.setValue(metadataData.base64URLEncodedString, forHTTPHeaderField: AppleArchiveTransfer.metadataHeader)
        let data: Data
        let response: URLResponse
        do {
            (data, response) = try await session.upload(for: request, fromFile: archiveURL)
        } catch {
            throw NodeClientError.transport(String(describing: error))
        }
        let http = try requireHTTPResponse(response)
        try validateHTTPResponse(data: data, response: http, expectedStatusCodes: [200])
        return try decode(BackupResponse.self, from: data)
    }

    public func verifySnapshot(_ request: SnapshotRequest) async throws -> VerifyResponse {
        try await send(path: "api/v1/backups/verify", method: "POST", body: request, timeout: 86_400)
    }

    public func previewRestore(_ request: RestorePreviewRequest) async throws -> RestorePlan {
        try await send(path: "api/v1/restores/preview", method: "POST", body: request)
    }

    public func previewArchiveRestore(
        backupId: UUID,
        snapshotId: String,
        conflictPolicy: ConflictPolicy,
        jobId: String
    ) async throws -> RestorePlan {
        try await send(
            path: "api/v1/restores/archive/preview",
            method: "POST",
            body: RestoreArchivePreviewRequest(
                backupId: backupId,
                snapshotId: snapshotId,
                conflictPolicy: conflictPolicy,
                jobId: jobId
            )
        )
    }

    public func executeRestore(_ plan: RestorePlan) async throws -> RestoreResponse {
        try await send(
            path: "api/v1/restores/execute",
            method: "POST",
            body: RestoreExecuteRequest(plan: plan),
            timeout: 86_400
        )
    }

    public func executeArchiveRestore(_ plan: RestorePlan, targetURL: URL) async throws -> RestoreResponse {
        try await Task.detached(priority: .userInitiated) {
            try AppleArchiveTransfer.requireEmptyDirectory(targetURL)
        }.value
        let body = try encoder.encode(RestoreExecuteRequest(plan: plan))
        var request = try authenticatedRequest(
            path: "api/v1/restores/archive/execute",
            method: "POST",
            accept: AppleArchiveTransfer.restoreContentType
        )
        request.timeoutInterval = 86_400
        request.httpBody = body
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        let downloadedURL: URL
        let response: URLResponse
        do {
            (downloadedURL, response) = try await session.download(for: request)
        } catch {
            throw NodeClientError.transport(String(describing: error))
        }
        let http = try requireHTTPResponse(response)
        guard http.statusCode == 200 else {
            let data = (try? boundedResponseData(at: downloadedURL)) ?? Data()
            try validateHTTPResponse(data: data, response: http, expectedStatusCodes: [200])
            throw NodeClientError.invalidResponse
        }
        let contentType = http.value(forHTTPHeaderField: "Content-Type")?
            .split(separator: ";", maxSplits: 1)
            .first?
            .trimmingCharacters(in: .whitespacesAndNewlines)
        guard contentType == AppleArchiveTransfer.restoreContentType,
              let encodedResult = http.value(forHTTPHeaderField: AppleArchiveTransfer.restoreResultHeader),
              let resultData = Data(base64URLEncoded: encodedResult)
        else {
            throw NodeClientError.invalidResponse
        }
        let result = try decode(RestoreResponse.self, from: resultData)
        let archiveURL = try await Task.detached(priority: .userInitiated) {
            try AppleArchiveTransfer.copyDownloadedArchive(downloadedURL)
        }.value
        defer { try? FileManager.default.removeItem(at: archiveURL) }
        try await Task.detached(priority: .userInitiated) {
            try AppleArchiveTransfer.extractRestoreArchive(archiveURL, to: targetURL, plan: plan)
        }.value
        return result
    }

    public func controlJob(jobId: String, action: JobAction) async throws -> JobControlResponse {
        try await send(
            path: "api/v1/jobs/control",
            method: "POST",
            body: JobControlRequest(jobId: jobId, action: action)
        )
    }

    private func send<Response: Decodable & Sendable>(
        path: String,
        method: String = "GET",
        authenticated: Bool = true,
        timeout: TimeInterval? = nil
    ) async throws -> Response {
        try await send(path: path, method: method, bodyData: nil, authenticated: authenticated, timeout: timeout)
    }

    private func send<Response: Decodable & Sendable, Body: Encodable & Sendable>(
        path: String,
        method: String,
        body: Body,
        authenticated: Bool = true,
        timeout: TimeInterval? = nil
    ) async throws -> Response {
        let data = try encoder.encode(body)
        return try await send(
            path: path,
            method: method,
            bodyData: data,
            authenticated: authenticated,
            timeout: timeout
        )
    }

    private func send<Response: Decodable & Sendable>(
        path: String,
        method: String,
        bodyData: Data?,
        authenticated: Bool,
        timeout: TimeInterval?
    ) async throws -> Response {
        let (data, _) = try await execute(
            path: path,
            method: method,
            bodyData: bodyData,
            authenticated: authenticated,
            timeout: timeout,
            expectedStatusCodes: [200]
        )
        do {
            return try decoder.decode(Response.self, from: data)
        } catch {
            throw NodeClientError.invalidPayload(String(describing: error))
        }
    }

    private func sendNoContent<Body: Encodable & Sendable>(path: String, body: Body) async throws {
        let data = try encoder.encode(body)
        _ = try await execute(
            path: path,
            method: "POST",
            bodyData: data,
            authenticated: true,
            timeout: nil,
            expectedStatusCodes: [204]
        )
    }

    private func execute(
        path: String,
        method: String,
        bodyData: Data?,
        authenticated: Bool,
        timeout: TimeInterval?,
        expectedStatusCodes: Set<Int>
    ) async throws -> (Data, HTTPURLResponse) {
        var request = authenticated || configuration.apiToken != nil
            ? try authenticatedRequest(path: path, method: method, accept: "application/json", authenticated: authenticated)
            : URLRequest(url: configuration.baseURL.appending(path: path))
        request.httpMethod = method
        request.timeoutInterval = timeout ?? 20
        request.cachePolicy = .reloadIgnoringLocalCacheData
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        request.setValue("no-store", forHTTPHeaderField: "Cache-Control")
        if let bodyData {
            request.httpBody = bodyData
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }
        let data: Data
        let response: URLResponse
        do {
            (data, response) = try await session.data(for: request)
        } catch {
            throw NodeClientError.transport(String(describing: error))
        }
        let http = try requireHTTPResponse(response)
        try validateHTTPResponse(data: data, response: http, expectedStatusCodes: expectedStatusCodes)
        return (data, http)
    }

    private func authenticatedRequest(
        path: String,
        method: String,
        accept: String,
        authenticated: Bool = true
    ) throws -> URLRequest {
        var request = URLRequest(url: configuration.baseURL.appending(path: path))
        request.httpMethod = method
        request.cachePolicy = .reloadIgnoringLocalCacheData
        request.setValue(accept, forHTTPHeaderField: "Accept")
        request.setValue("no-store", forHTTPHeaderField: "Cache-Control")
        if authenticated {
            guard let token = configuration.apiToken else { throw NodeClientError.missingToken }
            guard configuration.baseURL.scheme?.lowercased() == "https" || configuration.baseURL.isLoopback else {
                throw NodeClientError.insecureAuthenticatedTransport
            }
            request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        }
        return request
    }

    private func requireHTTPResponse(_ response: URLResponse) throws -> HTTPURLResponse {
        guard let response = response as? HTTPURLResponse else { throw NodeClientError.invalidResponse }
        return response
    }

    private func validateHTTPResponse(
        data: Data,
        response: HTTPURLResponse,
        expectedStatusCodes: Set<Int>
    ) throws {
        guard expectedStatusCodes.contains(response.statusCode) else {
            let payload = try? decoder.decode(APIErrorPayload.self, from: data)
            if let payload, payload.protocolVersion != covalentProtocolVersion {
                throw NodeClientError.unsupportedProtocol(payload.protocolVersion)
            }
            if response.statusCode == 401 { throw NodeClientError.unauthorized }
            throw NodeClientError.api(
                status: response.statusCode,
                code: payload?.code ?? "http_\(response.statusCode)",
                message: payload?.message ?? HTTPURLResponse.localizedString(forStatusCode: response.statusCode),
                retryable: payload?.retryable ?? (500...599).contains(response.statusCode)
            )
        }
    }

    private func decode<Response: Decodable>(_ type: Response.Type, from data: Data) throws -> Response {
        do {
            return try decoder.decode(type, from: data)
        } catch {
            throw NodeClientError.invalidPayload(String(describing: error))
        }
    }

    private func boundedResponseData(at url: URL) throws -> Data {
        let size = try url.resourceValues(forKeys: [.fileSizeKey]).fileSize ?? 0
        guard size <= 2 * 1_024 * 1_024 else { return Data() }
        return try Data(contentsOf: url)
    }
}

private extension URL {
    var isLoopback: Bool {
        guard let host = host(percentEncoded: false)?.lowercased() else { return false }
        return host == "localhost" || host == "127.0.0.1" || host == "::1" || host == "[::1]"
    }
}

private struct ConfigImportRequest: Codable, Sendable {
    let confirmed: Bool
    let settings: ExportedDeviceSettings
}

private struct PairInvitationRequest: Codable, Sendable {
    let lifetimeMs: UInt64
    let endpoints: [String]
}

private struct PairAcceptRequest: Codable, Sendable {
    let invitation: PairingInvitation
    let responderName: String
    let responderRoles: Set<PeerRole>
    let inviterRoles: Set<PeerRole>
}

private struct PairConfirmRequest: Codable, Sendable {
    let session: PairingSession
    let displayedCode: String
}

private struct PairFinalizeRequest: Codable, Sendable {
    let session: PairingSession
}

private struct PeerRequest: Codable, Sendable {
    let peerId: UUID
}

private struct ConnectProviderRequest: Codable, Sendable {
    let peerId: UUID
    let address: String
    let certificateDer: String
}

private struct RestoreExecuteRequest: Codable, Sendable {
    let plan: RestorePlan
}

private struct RestoreArchivePreviewRequest: Codable, Sendable {
    let backupId: UUID
    let snapshotId: String
    let conflictPolicy: ConflictPolicy
    let jobId: String
}

private struct JobControlRequest: Codable, Sendable {
    let jobId: String
    let action: JobAction
}

private extension Data {
    var base64URLEncodedString: String {
        base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .trimmingCharacters(in: CharacterSet(charactersIn: "="))
    }

    init?(base64URLEncoded value: String) {
        var normalized = value
            .replacingOccurrences(of: "-", with: "+")
            .replacingOccurrences(of: "_", with: "/")
        let remainder = normalized.count % 4
        if remainder != 0 { normalized += String(repeating: "=", count: 4 - remainder) }
        self.init(base64Encoded: normalized)
    }
}

public enum NodeClientError: Error, Equatable, Sendable {
    case invalidServiceURL
    case invalidToken
    case missingToken
    case insecureAuthenticatedTransport
    case invalidResponse
    case invalidPayload(String)
    case unsupportedProtocol(UInt16)
    case unauthorized
    case transport(String)
    case api(status: Int, code: String, message: String, retryable: Bool)
}

extension NodeClientError: LocalizedError {
    public var errorDescription: String? {
        switch self {
        case .invalidServiceURL: "Enter a complete HTTP or HTTPS service address."
        case .invalidToken: "The local API token is not valid."
        case .missingToken: "Connect this app with the node's local API token."
        case .insecureAuthenticatedTransport:
            "Covalent will not send its API token over plain HTTP to another device. Use loopback or HTTPS."
        case .invalidResponse: "The service returned an invalid response."
        case let .invalidPayload(details): "The service response did not match protocol 1. \(details)"
        case let .unsupportedProtocol(version): "This node uses unsupported protocol \(version)."
        case .unauthorized: "The local API token was rejected."
        case let .transport(message): "The Covalent service could not be reached. \(message)"
        case let .api(_, _, message, _): message
        }
    }
}
