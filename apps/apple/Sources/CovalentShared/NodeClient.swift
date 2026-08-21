import CryptoKit
import Darwin
import Foundation
import Security

public struct NodeConnectionConfiguration: Equatable, Sendable {
    public let baseURL: URL
    public let apiToken: String?
    public let trustedCertificateDER: Data?

    public init(baseURL: URL, apiToken: String?, trustedCertificateDER: Data? = nil) throws {
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
        if let trustedCertificateDER {
            guard scheme == "https",
                  trustedCertificateDER.count <= 64 * 1_024,
                  SecCertificateCreateWithData(nil, trustedCertificateDER as CFData) != nil
            else {
                throw NodeClientError.invalidTrustedCertificate
            }
        }
        self.baseURL = baseURL
        self.apiToken = token?.isEmpty == false ? token : nil
        self.trustedCertificateDER = trustedCertificateDER
    }

    public static var localDefault: Self {
        try! Self(baseURL: URL(string: "http://127.0.0.1:8787")!, apiToken: nil)
    }
}

public actor NodeClient {
    private let configuration: NodeConnectionConfiguration
    private let session: URLSession
    private let trustDelegate: PinnedServerTrustDelegate?
    private let decoder: JSONDecoder
    private let encoder: JSONEncoder
    private let transferDirectory: URL

    public init(
        configuration: NodeConnectionConfiguration = .localDefault,
        session: URLSession? = nil,
        transferDirectory: URL? = nil
    ) {
        self.configuration = configuration
        if let session {
            self.session = session
            self.trustDelegate = nil
        } else {
            let sessionConfiguration = URLSessionConfiguration.ephemeral
            sessionConfiguration.requestCachePolicy = .reloadIgnoringLocalCacheData
            sessionConfiguration.timeoutIntervalForRequest = 20
            sessionConfiguration.timeoutIntervalForResource = 3_600
            sessionConfiguration.urlCache = nil
            let delegate = configuration.trustedCertificateDER.flatMap(PinnedServerTrustDelegate.init(certificateDER:))
            self.trustDelegate = delegate
            self.session = URLSession(configuration: sessionConfiguration, delegate: delegate, delegateQueue: nil)
        }
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        self.decoder = decoder
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        encoder.dateEncodingStrategy = .iso8601
        self.encoder = encoder
        self.transferDirectory = transferDirectory ?? Self.defaultTransferDirectory()
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

    public func startNetworkPairing(candidateAddress: String) async throws -> NetworkPairing {
        try await send(
            path: "api/v1/pair/network/start",
            method: "POST",
            body: NetworkPairingStartRequest(candidateAddress: candidateAddress)
        )
    }

    public func pendingNetworkPairings() async throws -> [NetworkPairing] {
        try await send(path: "api/v1/pair/network/pending")
    }

    public func confirmNetworkPairing(
        pairingId: String,
        displayedCode: String
    ) async throws -> NetworkPairing {
        try validatePairingIdentifier(pairingId)
        return try await send(
            path: "api/v1/pair/network/\(pairingId)/confirm",
            method: "POST",
            body: NetworkPairingConfirmRequest(displayedCode: displayedCode)
        )
    }

    public func cancelNetworkPairing(pairingId: String) async throws {
        try validatePairingIdentifier(pairingId)
        _ = try await execute(
            path: "api/v1/pair/network/\(pairingId)",
            queryItems: [],
            method: "DELETE",
            bodyData: nil,
            authenticated: true,
            timeout: nil,
            expectedStatusCodes: [204]
        )
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

    public func connectProvider(using transport: PeerTransport) async throws -> ProviderConnection {
        try await send(
            path: "api/v1/providers/connect",
            method: "POST",
            body: ConnectProviderRequest(peerTransport: transport)
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

    public func createBackupArchive(
        sourceURL: URL,
        metadata: ArchiveBackupMetadata,
        onProgress: (@Sendable (TransferProgressSnapshot) -> Void)? = nil
    ) async throws -> BackupResponse {
        guard metadata.protocolVersion == covalentProtocolVersion else {
            throw NodeClientError.unsupportedProtocol(metadata.protocolVersion)
        }
        let metadataData = try encoder.encode(metadata)
        guard metadataData.count <= 32 * 1_024 else {
            throw NodeClientError.invalidPayload(NodeClientFailure(summary: "This backup's details are too large to send. Try a shorter backup name."))
        }
        // Staging reads and encrypts every file before a single byte is sent,
        // and its size is not known until it finishes, so this phase is
        // honestly indeterminate.
        onProgress?(TransferProgressSnapshot(phase: .preparing))
        var upload = try await prepareArchiveUpload(
            sourceURL: sourceURL,
            metadata: metadata,
            metadataData: metadataData
        )
        onProgress?(
            TransferProgressSnapshot(
                phase: .transferring,
                completedBytes: upload.offset,
                totalBytes: upload.length
            )
        )
        for _ in 0..<8 {
            var request = try authenticatedRequest(
                path: "api/v1/backups/archive",
                method: "POST",
                accept: "application/json"
            )
            request.timeoutInterval = 86_400
            request.setValue(AppleArchiveTransfer.backupContentType, forHTTPHeaderField: "Content-Type")
            request.setValue(metadataData.base64URLEncodedString, forHTTPHeaderField: AppleArchiveTransfer.metadataHeader)
            request.setValue(String(upload.offset), forHTTPHeaderField: AppleArchiveTransfer.uploadOffsetHeader)
            request.setValue(String(upload.length), forHTTPHeaderField: AppleArchiveTransfer.uploadLengthHeader)
            request.setValue(upload.digest, forHTTPHeaderField: AppleArchiveTransfer.uploadDigestHeader)
            request.setValue(String(upload.length - upload.offset), forHTTPHeaderField: "Content-Length")
            request.httpBodyStream = try ArchiveUploadBody.slice(
                archivePath: upload.archivePath,
                offset: upload.offset,
                count: upload.length - upload.offset
            )
            let data: Data
            let response: URLResponse
            do {
                if let onProgress {
                    let progress = TransferProgressDelegate(
                        baseOffset: upload.offset,
                        declaredTotal: upload.length,
                        report: onProgress
                    )
                    (data, response) = try await session.data(for: request, delegate: progress)
                } else {
                    (data, response) = try await session.data(for: request)
                }
            } catch {
                throw NodeClientError.transport(NodeTransportCopy.describe(error))
            }
            let http = try requireHTTPResponse(response)
            if http.statusCode == 409,
               let authoritative = http.value(forHTTPHeaderField: AppleArchiveTransfer.uploadOffsetHeader)
                    .flatMap(UInt64.init),
               authoritative <= upload.length
            {
                let payload = try? decoder.decode(APIErrorPayload.self, from: data)
                if let code = payload?.code,
                   payload?.retryable == true,
                   ["upload_offset_mismatch", "upload_incomplete"].contains(code)
                {
                    guard authoritative != upload.offset || upload.offset != upload.length else {
                        try validateHTTPResponse(data: data, response: http, expectedStatusCodes: [200])
                        throw NodeClientError.invalidResponse
                    }
                    upload.offset = authoritative
                    try persistArchiveUpload(upload)
                    continue
                }
            }
            try validateHTTPResponse(data: data, response: http, expectedStatusCodes: [200])
            guard http.value(forHTTPHeaderField: AppleArchiveTransfer.jobAcknowledgementRequiredHeader) == "true" else {
                throw NodeClientError.invalidResponse
            }
            upload.offset = upload.length
            try persistArchiveUpload(upload)
            onProgress?(
                TransferProgressSnapshot(
                    phase: .finishing,
                    completedBytes: upload.length,
                    totalBytes: upload.length
                )
            )
            return try decode(BackupResponse.self, from: data)
        }
        throw NodeClientError.transport(NodeClientFailure(summary: "This upload kept losing its place. Start the backup again.", recovery: .retry))
    }

    public func verifySnapshot(_ request: SnapshotRequest) async throws -> VerifyResponse {
        try await send(path: "api/v1/backups/verify", method: "POST", body: request, timeout: 86_400)
    }

    public func previewRestore(_ request: RestorePreviewRequest) async throws -> RestorePlan {
        let reference = try await previewRestoreReference(
            path: "api/v1/restores/preview",
            body: request
        )
        return try await materializeRestorePlan(reference)
    }

    public func previewArchiveRestore(
        backupId: UUID,
        snapshotId: String,
        conflictPolicy: ConflictPolicy,
        jobId: String,
        targetURL: URL
    ) async throws -> RestorePlan {
        let inventory = try await uploadTargetInventory(targetURL: targetURL, jobId: jobId)
        let reference = try await previewRestoreReference(
            path: "api/v1/restores/archive/preview",
            body: RestoreArchivePreviewRequest(
                backupId: backupId,
                snapshotId: snapshotId,
                conflictPolicy: conflictPolicy,
                jobId: jobId,
                targetInventoryId: inventory.reference.inventoryId
            )
        )
        return try await materializeRestorePlan(reference)
    }

    public func executeRestore(_ plan: RestorePlan) async throws -> RestoreResponse {
        let body = try encoder.encode(RestoreExecuteRequest(planId: plan.planId))
        let (data, response) = try await execute(
            path: "api/v1/restores/execute",
            queryItems: [],
            method: "POST",
            bodyData: body,
            authenticated: true,
            timeout: 86_400,
            expectedStatusCodes: [200]
        )
        try validateRestorePlanHeaders(response, reference: plan.reference)
        return try decode(RestoreResponse.self, from: data)
    }

    public func executeArchiveRestore(
        _ plan: RestorePlan,
        targetURL: URL,
        onProgress: (@Sendable (TransferProgressSnapshot) -> Void)? = nil
    ) async throws -> RestoreResponse {
        onProgress?(TransferProgressSnapshot(phase: .preparing))
        let freshInventory = try await uploadTargetInventory(targetURL: targetURL, jobId: plan.jobId)
        let reboundReference = try await previewRestoreReference(
            path: "api/v1/restores/archive/preview",
            body: RestoreArchivePreviewRequest(
                backupId: plan.backupId,
                snapshotId: plan.snapshotId,
                conflictPolicy: plan.conflictPolicy,
                jobId: plan.jobId,
                targetInventoryId: freshInventory.reference.inventoryId
            )
        )
        guard reboundReference == plan.reference else {
            throw NodeClientError.invalidPayload(NodeClientFailure(summary: "The folder you're restoring into changed since you previewed it. Preview the restore again.", recovery: .previewRestoreAgain))
        }
        let body = try encoder.encode(RestoreExecuteRequest(planId: plan.planId))
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
            if let onProgress {
                // The signed plan states an entry count, not a byte count, so
                // the denominator comes from the response's content length.
                // When the server sends none, the total stays nil and the UI
                // falls back to an indeterminate bar rather than guessing.
                let progress = TransferProgressDelegate(observesTaskProgress: true, report: onProgress)
                (downloadedURL, response) = try await session.download(for: request, delegate: progress)
            } else {
                (downloadedURL, response) = try await session.download(for: request)
            }
        } catch {
            throw NodeClientError.transport(NodeTransportCopy.describe(error))
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
              let resultData = Data(base64URLEncoded: encodedResult),
              http.value(forHTTPHeaderField: AppleArchiveTransfer.restorePlanIdHeader) == plan.planId,
              http.value(forHTTPHeaderField: AppleArchiveTransfer.restorePlanDigestHeader) == plan.planDigest,
              http.value(forHTTPHeaderField: AppleArchiveTransfer.jobAcknowledgementRequiredHeader) == "true"
        else {
            throw NodeClientError.invalidResponse
        }
        let result = try decode(RestoreResponse.self, from: resultData)
        // The signed result carries the authoritative byte count, so the bar
        // lands on a true 100% before local extraction begins.
        onProgress?(
            TransferProgressSnapshot(
                phase: .finishing,
                completedBytes: result.bytesWritten,
                totalBytes: result.bytesWritten
            )
        )
        let archiveURL = try await Task.detached(priority: .userInitiated) {
            try AppleArchiveTransfer.copyDownloadedArchive(downloadedURL)
        }.value
        defer { try? FileManager.default.removeItem(at: archiveURL) }
        try await Task.detached(priority: .userInitiated) {
            try AppleArchiveTransfer.extractRestoreArchive(
                archiveURL,
                to: targetURL,
                plan: plan,
                expectedInventory: freshInventory.draft
            )
        }.value
        return result
    }

    private func uploadTargetInventory(
        targetURL: URL,
        jobId: String
    ) async throws -> UploadedTargetInventory {
        let draft = try await Task.detached(priority: .userInitiated) {
            try AppleArchiveTransfer.makeTargetInventory(targetURL: targetURL)
        }.value
        let started: TargetInventoryUploadResponse = try await send(
            path: "api/v1/restores/archive/inventories",
            method: "POST",
            body: BeginTargetInventoryRequest(
                jobId: jobId,
                schemaVersion: 1,
                rootIdentity: draft.rootIdentity,
                entryCount: UInt64(draft.entries.count),
                totalBytes: draft.totalBytes
            )
        )
        var offset = 0
        let pageSize = 5_000
        while offset < draft.entries.count {
            let end = min(offset + pageSize, draft.entries.count)
            let entries = Array(draft.entries[offset..<end])
            let response: TargetInventoryUploadResponse = try await send(
                path: "api/v1/restores/archive/inventories/\(started.inventoryId)/pages",
                method: "POST",
                body: TargetInventoryPageRequest(
                    jobId: jobId,
                    offset: UInt64(offset),
                    pageDigest: Self.targetInventoryPageDigest(entries),
                    entries: entries
                )
            )
            guard response.inventoryId == started.inventoryId,
                  response.jobId == jobId,
                  response.nextOffset == UInt64(end)
            else { throw NodeClientError.invalidResponse }
            offset = end
        }
        let finalized: TargetInventoryReference = try await send(
            path: "api/v1/restores/archive/inventories/\(started.inventoryId)/finalize",
            method: "POST",
            body: FinalizeTargetInventoryRequest(
                jobId: jobId,
                entryCount: UInt64(draft.entries.count),
                totalBytes: draft.totalBytes,
                inventoryDigest: ""
            )
        )
        guard finalized.inventoryId == started.inventoryId,
              finalized.jobId == jobId,
              finalized.rootIdentity == draft.rootIdentity,
              finalized.entryCount == UInt64(draft.entries.count),
              finalized.totalBytes == draft.totalBytes,
              finalized.inventoryDigest.utf8.count == 64
        else { throw NodeClientError.invalidResponse }
        return UploadedTargetInventory(reference: finalized, draft: draft)
    }

    private static func targetInventoryPageDigest(_ entries: [TargetInventoryEntry]) -> String {
        var digest = SHA256()
        digest.update(data: Data("covalent/target-inventory-page/v1".utf8))
        digest.update(data: bigEndian(UInt64(entries.count)))
        for entry in entries {
            let path = Data(entry.path.utf8)
            digest.update(data: bigEndian(UInt64(path.count)))
            digest.update(data: path)
            digest.update(data: Data([entry.kind == .file ? 1 : 2]))
            digest.update(data: bigEndian(entry.length))
            if let modified = entry.modifiedAtUnixMs {
                digest.update(data: Data([1]))
                digest.update(data: bigEndian(modified))
            } else {
                digest.update(data: Data([0]))
            }
            let identity = Data(entry.identityToken.utf8)
            digest.update(data: bigEndian(UInt64(identity.count)))
            digest.update(data: identity)
        }
        return digest.finalize().map { String(format: "%02x", $0) }.joined()
    }

    private static func bigEndian(_ value: UInt64) -> Data {
        var value = value.bigEndian
        return withUnsafeBytes(of: &value) { Data($0) }
    }

    public func acknowledgeJob(jobId: String) async throws {
        try await sendNoContent(path: "api/v1/jobs/acknowledge", body: JobReferenceRequest(jobId: jobId))
        try removeArchiveUpload(jobId: jobId)
    }

    public func discardJob(jobId: String) async throws {
        try await sendNoContent(path: "api/v1/jobs/discard", body: JobReferenceRequest(jobId: jobId))
        try removeArchiveUpload(jobId: jobId)
    }

    public func controlJob(jobId: String, action: JobAction) async throws -> JobControlResponse {
        try await send(
            path: "api/v1/jobs/control",
            method: "POST",
            body: JobControlRequest(jobId: jobId, action: action)
        )
    }

    private func materializeRestorePlan(_ reference: RestorePlanReference) async throws -> RestorePlan {
        guard reference.planId.isLowercaseHexDigest,
              reference.planDigest.isLowercaseHexDigest,
              reference.manifestDigest.isLowercaseHexDigest,
              !reference.signature.isEmpty,
              (0...AppleArchiveTransfer.maximumEntries).contains(reference.totalEntries)
        else {
            throw NodeClientError.invalidPayload(NodeClientFailure(summary: "This restore is too large for Covalent to plan in one pass on this device. Restore a smaller backup."))
        }
        var entries: [RestorePreviewEntry] = []
        entries.reserveCapacity(reference.totalEntries)
        var cursor: String?
        repeat {
            var queryItems = [URLQueryItem(name: "limit", value: "1000")]
            if let cursor { queryItems.append(URLQueryItem(name: "cursor", value: cursor)) }
            let page: RestorePlanPage = try await send(
                path: "api/v1/restores/plans/\(reference.planId)",
                queryItems: queryItems
            )
            guard page.matches(reference),
                  page.entryOffset == entries.count,
                  page.entries.count <= 1_000,
                  entries.count + page.entries.count <= reference.totalEntries
            else {
                throw NodeClientError.invalidPayload(NodeClientFailure(summary: "Covalent lost its place while reading the restore plan. Preview the restore again.", recovery: .previewRestoreAgain))
            }
            if let nextCursor = page.nextCursor, nextCursor == cursor {
                throw NodeClientError.invalidPayload(NodeClientFailure(summary: "Covalent lost its place while reading the restore plan. Preview the restore again.", recovery: .previewRestoreAgain))
            }
            entries.append(contentsOf: page.entries)
            cursor = page.nextCursor
        } while cursor != nil
        guard entries.count == reference.totalEntries else {
            throw NodeClientError.invalidPayload(NodeClientFailure(summary: "The restore plan arrived incomplete. Preview the restore again.", recovery: .previewRestoreAgain))
        }
        return RestorePlan(reference: reference, entries: entries)
    }

    private func previewRestoreReference<Body: Encodable & Sendable>(
        path: String,
        body: Body
    ) async throws -> RestorePlanReference {
        let bodyData = try encoder.encode(body)
        let (data, response) = try await execute(
            path: path,
            queryItems: [],
            method: "POST",
            bodyData: bodyData,
            authenticated: true,
            timeout: nil,
            expectedStatusCodes: [200]
        )
        let reference = try decode(RestorePlanReference.self, from: data)
        try validateRestorePlanHeaders(response, reference: reference)
        guard response.value(forHTTPHeaderField: "Cache-Control")?.lowercased().contains("no-store") == true else {
            throw NodeClientError.invalidResponse
        }
        return reference
    }

    private func validateRestorePlanHeaders(
        _ response: HTTPURLResponse,
        reference: RestorePlanReference
    ) throws {
        guard response.value(forHTTPHeaderField: AppleArchiveTransfer.restorePlanIdHeader) == reference.planId,
              response.value(forHTTPHeaderField: AppleArchiveTransfer.restorePlanDigestHeader) == reference.planDigest
        else {
            throw NodeClientError.invalidResponse
        }
    }

    private func send<Response: Decodable & Sendable>(
        path: String,
        queryItems: [URLQueryItem] = [],
        method: String = "GET",
        authenticated: Bool = true,
        timeout: TimeInterval? = nil
    ) async throws -> Response {
        try await send(
            path: path,
            queryItems: queryItems,
            method: method,
            bodyData: nil,
            authenticated: authenticated,
            timeout: timeout
        )
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
            queryItems: [],
            method: method,
            bodyData: data,
            authenticated: authenticated,
            timeout: timeout
        )
    }

    private func prepareArchiveUpload(
        sourceURL: URL,
        metadata: ArchiveBackupMetadata,
        metadataData: Data
    ) async throws -> DurableArchiveUpload {
        try Self.prepareTransferDirectory(transferDirectory)
        let recordURL = archiveUploadRecordURL(jobId: metadata.jobId)
        let metadataDigest = Self.sha256Hex(metadataData)
        if FileManager.default.fileExists(atPath: recordURL.path) {
            let record = try decoder.decode(
                DurableArchiveUpload.self,
                from: Data(contentsOf: recordURL, options: [.mappedIfSafe])
            )
            guard record.schemaVersion == 1,
                  record.jobId == metadata.jobId,
                  record.metadataDigest == metadataDigest,
                  record.offset <= record.length
            else {
                throw NodeClientError.invalidPayload(NodeClientFailure(summary: "The saved progress for this backup can't be trusted. Start the backup again.", recovery: .retry))
            }
            let identity = try await Task.detached(priority: .userInitiated) {
                try AppleArchiveTransfer.uploadIdentity(for: URL(fileURLWithPath: record.archivePath))
            }.value
            guard identity.length == record.length, identity.digest == record.digest else {
                throw NodeClientError.invalidPayload(NodeClientFailure(summary: "The files changed while this backup was interrupted. Start the backup again.", recovery: .retry))
            }
            return record
        }
        let temporary = try await Task.detached(priority: .userInitiated) {
            try AppleArchiveTransfer.makeBackupArchive(sourceURL: sourceURL)
        }.value
        let identity = try await Task.detached(priority: .userInitiated) {
            try AppleArchiveTransfer.uploadIdentity(for: temporary)
        }.value
        let archiveURL = transferDirectory.appending(
            path: "upload-\(Self.jobToken(metadata.jobId)).zip",
            directoryHint: .notDirectory
        )
        do {
            try FileManager.default.moveItem(at: temporary, to: archiveURL)
            try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: archiveURL.path)
        } catch {
            try? FileManager.default.removeItem(at: temporary)
            throw error
        }
        let record = DurableArchiveUpload(
            schemaVersion: 1,
            jobId: metadata.jobId,
            metadataDigest: metadataDigest,
            archivePath: archiveURL.path,
            length: identity.length,
            digest: identity.digest,
            offset: 0
        )
        try persistArchiveUpload(record)
        return record
    }

    private func persistArchiveUpload(_ upload: DurableArchiveUpload) throws {
        try Self.prepareTransferDirectory(transferDirectory)
        let data = try encoder.encode(upload)
        let recordURL = archiveUploadRecordURL(jobId: upload.jobId)
        #if os(iOS)
        let writeOptions: Data.WritingOptions = [.atomic, .completeFileProtection]
        #else
        let writeOptions: Data.WritingOptions = [.atomic]
        #endif
        try data.write(to: recordURL, options: writeOptions)
        try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: recordURL.path)
    }

    private func removeArchiveUpload(jobId: String) throws {
        let recordURL = archiveUploadRecordURL(jobId: jobId)
        guard FileManager.default.fileExists(atPath: recordURL.path) else { return }
        let data = try Data(contentsOf: recordURL, options: [.mappedIfSafe])
        let upload = try decoder.decode(DurableArchiveUpload.self, from: data)
        guard upload.jobId == jobId,
              URL(fileURLWithPath: upload.archivePath).deletingLastPathComponent().standardizedFileURL
                == transferDirectory.standardizedFileURL
        else {
            throw NodeClientError.invalidPayload(NodeClientFailure(summary: "The saved progress for this backup can't be trusted. Start the backup again.", recovery: .retry))
        }
        try? FileManager.default.removeItem(atPath: upload.archivePath)
        try FileManager.default.removeItem(at: recordURL)
    }

    private func archiveUploadRecordURL(jobId: String) -> URL {
        transferDirectory.appending(
            path: "upload-\(Self.jobToken(jobId)).json",
            directoryHint: .notDirectory
        )
    }

    private static func defaultTransferDirectory() -> URL {
        let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
            ?? FileManager.default.temporaryDirectory
        return base.appending(path: "Covalent/Transfers", directoryHint: .isDirectory)
    }

    private static func prepareTransferDirectory(_ directory: URL) throws {
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        try FileManager.default.setAttributes([.posixPermissions: 0o700], ofItemAtPath: directory.path)
        var mutable = directory
        var values = URLResourceValues()
        values.isExcludedFromBackup = true
        try? mutable.setResourceValues(values)
    }

    private static func jobToken(_ jobId: String) -> String {
        sha256Hex(Data(jobId.utf8))
    }

    private static func sha256Hex(_ data: Data) -> String {
        SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
    }

    private func send<Response: Decodable & Sendable>(
        path: String,
        queryItems: [URLQueryItem],
        method: String,
        bodyData: Data?,
        authenticated: Bool,
        timeout: TimeInterval?
    ) async throws -> Response {
        let (data, _) = try await execute(
            path: path,
            queryItems: queryItems,
            method: method,
            bodyData: bodyData,
            authenticated: authenticated,
            timeout: timeout,
            expectedStatusCodes: [200]
        )
        do {
            return try decoder.decode(Response.self, from: data)
        } catch {
            throw NodeClientError.invalidPayload(NodeTransportCopy.describeDecodingFailure(error))
        }
    }

    private func sendNoContent<Body: Encodable & Sendable>(path: String, body: Body) async throws {
        let data = try encoder.encode(body)
        _ = try await execute(
            path: path,
            queryItems: [],
            method: "POST",
            bodyData: data,
            authenticated: true,
            timeout: nil,
            expectedStatusCodes: [204]
        )
    }

    private func execute(
        path: String,
        queryItems: [URLQueryItem],
        method: String,
        bodyData: Data?,
        authenticated: Bool,
        timeout: TimeInterval?,
        expectedStatusCodes: Set<Int>
    ) async throws -> (Data, HTTPURLResponse) {
        var request = authenticated || configuration.apiToken != nil
            ? try authenticatedRequest(
                path: path,
                queryItems: queryItems,
                method: method,
                accept: "application/json",
                authenticated: authenticated
            )
            : URLRequest(url: try serviceURL(path: path, queryItems: queryItems))
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
            throw NodeClientError.transport(NodeTransportCopy.describe(error))
        }
        let http = try requireHTTPResponse(response)
        try validateHTTPResponse(data: data, response: http, expectedStatusCodes: expectedStatusCodes)
        return (data, http)
    }

    private func authenticatedRequest(
        path: String,
        queryItems: [URLQueryItem] = [],
        method: String,
        accept: String,
        authenticated: Bool = true
    ) throws -> URLRequest {
        var request = URLRequest(url: try serviceURL(path: path, queryItems: queryItems))
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

    private func serviceURL(path: String, queryItems: [URLQueryItem]) throws -> URL {
        let url = configuration.baseURL.appending(path: path)
        guard !queryItems.isEmpty else { return url }
        guard var components = URLComponents(url: url, resolvingAgainstBaseURL: false) else {
            throw NodeClientError.invalidServiceURL
        }
        components.queryItems = queryItems
        guard let result = components.url else { throw NodeClientError.invalidServiceURL }
        return result
    }

    private func validatePairingIdentifier(_ pairingId: String) throws {
        guard !pairingId.isEmpty,
              pairingId.utf8.count <= 128,
              pairingId.allSatisfy({ $0.isASCII && ($0.isLetter || $0.isNumber || $0 == "-" || $0 == "_") })
        else {
            throw NodeClientError.invalidPayload(NodeClientFailure(summary: "That pairing request isn't one Covalent recognises. Start pairing again.", recovery: .chooseAnotherDevice))
        }
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
            throw NodeClientError.invalidPayload(NodeTransportCopy.describeDecodingFailure(error))
        }
    }

    private func boundedResponseData(at url: URL) throws -> Data {
        let size = try url.resourceValues(forKeys: [.fileSizeKey]).fileSize ?? 0
        guard size <= 2 * 1_024 * 1_024 else { return Data() }
        return try Data(contentsOf: url)
    }
}

/// The pinning decision itself, separated from the URLSession plumbing that
/// delivers it.
///
/// It lives apart so it can be tested without a TLS server: the only test
/// that ever exercised pinning end to end needs a packaged Caddy, four
/// environment variables and a driver script, and for its whole life it
/// reported "passed" while running nothing. `PinnedTrustTests` drives this
/// function against a real certificate chain and gets the same answer the
/// delegate below would give, with no infrastructure at all.
enum PinnedTrust {
    /// Whether `trust` chains to `anchor` — and only to `anchor` — and is
    /// valid for `host`.
    ///
    /// `SecTrustSetAnchorCertificatesOnly(true)` is what makes this pinning
    /// rather than an addition: the system roots are taken out of
    /// consideration, so a certificate signed by any public CA is refused
    /// just as firmly as a self-signed one.
    static func accepts(_ trust: SecTrust, host: String, anchor: SecCertificate) -> Bool {
        let hostnamePolicy = SecPolicyCreateSSL(true, host as CFString)
        guard SecTrustSetPolicies(trust, hostnamePolicy) == errSecSuccess,
              SecTrustSetAnchorCertificates(trust, [anchor] as CFArray) == errSecSuccess,
              SecTrustSetAnchorCertificatesOnly(trust, true) == errSecSuccess
        else {
            return false
        }
        return SecTrustEvaluateWithError(trust, nil)
    }
}

private final class PinnedServerTrustDelegate: NSObject, URLSessionDelegate, @unchecked Sendable {
    private let certificate: SecCertificate

    init?(certificateDER: Data) {
        guard let certificate = SecCertificateCreateWithData(nil, certificateDER as CFData) else {
            return nil
        }
        self.certificate = certificate
    }

    func urlSession(
        _ session: URLSession,
        didReceive challenge: URLAuthenticationChallenge,
        completionHandler: @escaping @Sendable (URLSession.AuthChallengeDisposition, URLCredential?) -> Void
    ) {
        guard challenge.protectionSpace.authenticationMethod == NSURLAuthenticationMethodServerTrust,
              let trust = challenge.protectionSpace.serverTrust
        else {
            completionHandler(.performDefaultHandling, nil)
            return
        }
        guard PinnedTrust.accepts(trust, host: challenge.protectionSpace.host, anchor: certificate) else {
            completionHandler(.cancelAuthenticationChallenge, nil)
            return
        }
        completionHandler(.useCredential, URLCredential(trust: trust))
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

private struct NetworkPairingStartRequest: Codable, Sendable {
    let candidateAddress: String
}

private struct NetworkPairingConfirmRequest: Codable, Sendable {
    let displayedCode: String
}

private struct PairAcceptRequest: Codable, Sendable {
    let invitation: PairingInvitation
    let responderName: String
    let responderRoles: Set<PeerRole>
    let inviterRoles: Set<PeerRole>
}

private struct DurableArchiveUpload: Codable, Sendable {
    let schemaVersion: UInt16
    let jobId: String
    let metadataDigest: String
    let archivePath: String
    let length: UInt64
    let digest: String
    var offset: UInt64
}

/// Builds the request body for a resumable archive upload.
///
/// `URLSession` hands `httpBodyStream` to CFNetwork, which toll-free-bridges it to a
/// `CFReadStream` and calls `CFReadStreamSetClient` on it. `InputStream` is a class
/// cluster, so a Swift subclass that only overrides the read primitives reaches
/// `_NSRequestConcreteObject` ("-setDelegate: only defined for abstract class") and
/// aborts the process the moment CFNetwork starts providing the body. The body must
/// therefore be a concrete Foundation file stream, positioned through the public
/// `fileCurrentOffsetKey` property.
///
/// The archive is still validated through an `O_NOFOLLOW` descriptor, and the length
/// check is now made before the request is sent instead of aborting mid-body.
/// Turns `URLSession`'s byte-level callbacks into ``TransferProgressSnapshot``.
///
/// Attached per request via `data(for:delegate:)` / `download(for:delegate:)`
/// so it never disturbs the session-wide certificate-pinning delegate, and so
/// an injected test session is unaffected.
/// Deliberately conforms only to `URLSessionTaskDelegate`. Adopting
/// `URLSessionDownloadDelegate` would oblige it to implement
/// `didFinishDownloadingTo`, whose contract deletes the downloaded file once
/// the method returns — the async `download(for:delegate:)` owns that handoff,
/// and competing with it is not worth a progress bar. Download progress comes
/// from the task's own `Progress` instead, observed via `didCreateTask`.
private final class TransferProgressDelegate: NSObject, URLSessionTaskDelegate, @unchecked Sendable {
    private let baseOffset: UInt64
    private let declaredTotal: UInt64?
    private let report: @Sendable (TransferProgressSnapshot) -> Void
    private let observesTaskProgress: Bool
    private let lock = NSLock()
    private var progressObservation: NSKeyValueObservation?

    init(
        baseOffset: UInt64 = 0,
        declaredTotal: UInt64? = nil,
        observesTaskProgress: Bool = false,
        report: @escaping @Sendable (TransferProgressSnapshot) -> Void
    ) {
        self.baseOffset = baseOffset
        self.declaredTotal = declaredTotal
        self.observesTaskProgress = observesTaskProgress
        self.report = report
    }

    deinit {
        progressObservation?.invalidate()
    }

    func urlSession(
        _ session: URLSession,
        task: URLSessionTask,
        didSendBodyData bytesSent: Int64,
        totalBytesSent: Int64,
        totalBytesExpectedToSend: Int64
    ) {
        // A resumed upload sends only the remaining suffix, so both halves are
        // offset by whatever the durable record already proved was delivered.
        let completed = baseOffset &+ UInt64(max(0, totalBytesSent))
        let total = declaredTotal
            ?? (totalBytesExpectedToSend > 0 ? baseOffset &+ UInt64(totalBytesExpectedToSend) : nil)
        report(
            TransferProgressSnapshot(phase: .transferring, completedBytes: completed, totalBytes: total)
        )
    }

    /// Downloads report no body-data callback to a plain task delegate, so
    /// observe the task's `Progress`, which carries received/expected bytes.
    func urlSession(_ session: URLSession, didCreateTask task: URLSessionTask) {
        guard observesTaskProgress else { return }
        let observation = task.progress.observe(\.completedUnitCount, options: [.initial, .new]) {
            [report] progress, _ in
            let total = progress.totalUnitCount > 0 ? UInt64(progress.totalUnitCount) : nil
            report(
                TransferProgressSnapshot(
                    phase: .transferring,
                    completedBytes: UInt64(max(0, progress.completedUnitCount)),
                    totalBytes: total
                )
            )
        }
        lock.lock()
        progressObservation = observation
        lock.unlock()
    }
}

private enum ArchiveUploadBody {
    /// Streams `count` bytes of the archive at `archivePath`, starting at `offset`.
    static func slice(archivePath: String, offset: UInt64, count: UInt64) throws -> InputStream {
        let limit = UInt64(Int64.max)
        guard offset <= limit, count <= limit, offset <= limit - count else {
            throw NodeClientError.invalidPayload(NodeClientFailure(summary: "This backup is too large for this device to upload in one piece. Try backing up a smaller folder."))
        }
        let required = offset + count

        let descriptor = Darwin.open(archivePath, O_RDONLY | O_NOFOLLOW | O_CLOEXEC)
        guard descriptor >= 0 else {
            throw NodeClientError.invalidPayload(NodeClientFailure(summary: "Covalent couldn't reopen the backup it staged. Start the backup again.", recovery: .retry))
        }
        defer { Darwin.close(descriptor) }

        var opened = stat()
        guard fstat(descriptor, &opened) == 0, opened.st_mode & S_IFMT == S_IFREG else {
            throw NodeClientError.invalidPayload(NodeClientFailure(summary: "Covalent couldn't reopen the backup it staged. Start the backup again.", recovery: .retry))
        }
        guard opened.st_size >= 0, UInt64(opened.st_size) >= required else {
            throw NodeClientError.invalidPayload(NodeClientFailure(summary: "The staged backup file is incomplete. Start the backup again.", recovery: .retry))
        }

        // Foundation opens the stream by path, so confirm the path still names the exact
        // regular file the descriptor just vouched for rather than a symlink swapped in.
        var byPath = stat()
        guard lstat(archivePath, &byPath) == 0,
              byPath.st_mode & S_IFMT == S_IFREG,
              byPath.st_dev == opened.st_dev,
              byPath.st_ino == opened.st_ino,
              let stream = InputStream(url: URL(fileURLWithPath: archivePath))
        else {
            throw NodeClientError.invalidPayload(NodeClientFailure(summary: "Covalent couldn't reopen the backup it staged. Start the backup again.", recovery: .retry))
        }
        stream.setProperty(NSNumber(value: offset), forKey: .fileCurrentOffsetKey)
        return stream
    }
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
    let peerTransport: PeerTransport
}

private struct RestoreExecuteRequest: Codable, Sendable {
    let planId: String
}

private struct RestoreArchivePreviewRequest: Codable, Sendable {
    let backupId: UUID
    let snapshotId: String
    let conflictPolicy: ConflictPolicy
    let jobId: String
    let targetInventoryId: String
}

private struct BeginTargetInventoryRequest: Codable, Sendable {
    let jobId: String
    let schemaVersion: UInt16
    let rootIdentity: String
    let entryCount: UInt64
    let totalBytes: UInt64
}

private struct TargetInventoryPageRequest: Codable, Sendable {
    let jobId: String
    let offset: UInt64
    let pageDigest: String
    let entries: [TargetInventoryEntry]
}

private struct FinalizeTargetInventoryRequest: Codable, Sendable {
    let jobId: String
    let entryCount: UInt64
    let totalBytes: UInt64
    let inventoryDigest: String
}

private struct TargetInventoryUploadResponse: Codable, Sendable {
    let inventoryId: String
    let jobId: String
    let nextOffset: UInt64
}

private struct TargetInventoryReference: Codable, Sendable {
    let inventoryId: String
    let jobId: String
    let schemaVersion: UInt16
    let rootIdentity: String
    let entryCount: UInt64
    let totalBytes: UInt64
    let inventoryDigest: String
}

private struct UploadedTargetInventory: Sendable {
    let reference: TargetInventoryReference
    let draft: AppleArchiveTransfer.TargetInventoryDraft
}

private struct JobControlRequest: Codable, Sendable {
    let jobId: String
    let action: JobAction
}

private struct JobReferenceRequest: Codable, Sendable {
    let jobId: String
}

private extension RestorePlanPage {
    func matches(_ reference: RestorePlanReference) -> Bool {
        planId == reference.planId
            && backupId == reference.backupId
            && snapshotId == reference.snapshotId
            && authorizedRoot == reference.authorizedRoot
            && manifestDigest == reference.manifestDigest
            && conflictPolicy == reference.conflictPolicy
            && jobId == reference.jobId
            && planDigest == reference.planDigest
            && signerDeviceId == reference.signerDeviceId
            && signature == reference.signature
            && totalEntries == reference.totalEntries
            && targetInventory == reference.targetInventory
    }
}

private extension String {
    var isLowercaseHexDigest: Bool {
        utf8.count == 64 && utf8.allSatisfy { byte in
            (48...57).contains(byte) || (97...102).contains(byte)
        }
    }
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
    case invalidTrustedCertificate
    case missingToken
    case insecureAuthenticatedTransport
    case invalidResponse
    case invalidPayload(NodeClientFailure)
    case unsupportedProtocol(UInt16)
    case unauthorized
    case transport(NodeClientFailure)
    case api(status: Int, code: String, message: String, retryable: Bool)
}

extension NodeClientError: LocalizedError {
    /// Plain-English copy only. Technical text lives in ``diagnosticDetail``.
    public var errorDescription: String? {
        switch self {
        case .invalidServiceURL: "Enter a complete web address for your backup server, starting with http or https."
        case .invalidToken: "That access token isn't one Covalent can use."
        case .invalidTrustedCertificate: "Choose a valid DER or PEM certificate for an HTTPS server."
        case .missingToken: "Connect this app to your backup server before continuing."
        case .insecureAuthenticatedTransport:
            "Covalent won't send its access token over an unencrypted connection to another device. "
                + "Use an HTTPS address."
        case .invalidResponse: "Your backup server sent back something Covalent couldn't read."
        case let .invalidPayload(failure): failure.summary
        case .unsupportedProtocol:
            "This app and your backup server are running versions that can't work together. Update both."
        case .unauthorized: "This app is no longer signed in to your backup server. Reconnect it to continue."
        case let .transport(failure): failure.summary
        case let .api(status, code, message, retryable):
            NodeAPIErrorCopy.describe(status: status, code: code, message: message, retryable: retryable).summary
        }
    }

    /// The underlying technical text, kept for a "Details" disclosure and for
    /// logs. Never lead with this — ``errorDescription`` is what users read.
    public var diagnosticDetail: String? {
        switch self {
        case let .invalidPayload(failure), let .transport(failure): failure.detail
        case let .unsupportedProtocol(version): "unsupported protocol version \(version)"
        case let .api(status, code, message, retryable):
            NodeAPIErrorCopy.describe(status: status, code: code, message: message, retryable: retryable).detail
        default: nil
        }
    }

    /// What the person can do about this, surfaced as an alert button.
    public var recoveryHint: RecoveryHint {
        switch self {
        case .invalidServiceURL, .invalidToken, .invalidTrustedCertificate,
             .missingToken, .insecureAuthenticatedTransport, .unauthorized:
            .reconnect
        case .unsupportedProtocol: .none
        case .invalidResponse: .retry
        case let .invalidPayload(failure), let .transport(failure): failure.recovery
        case let .api(status, code, message, retryable):
            NodeAPIErrorCopy.describe(status: status, code: code, message: message, retryable: retryable).recovery
        }
    }
}
