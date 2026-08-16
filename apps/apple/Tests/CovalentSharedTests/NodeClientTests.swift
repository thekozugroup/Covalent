import Foundation
import Testing
@testable import CovalentShared

@Test func statusUsesPublicEndpointAndValidatesProtocol() async throws {
    let recorder = RequestRecorder { request in
        #expect(request.url?.path == "/api/v1/status")
        #expect(request.value(forHTTPHeaderField: "Authorization") == nil)
        return TestResponse.response(
            request,
            status: 200,
            json: #"{"deviceName":"Test Mac","protocolVersion":1,"lanDiscovery":true,"platformTier":"tier1","state":"ready"}"#
        )
    }
    let client = try makeClient(recorder: recorder, token: nil)
    let status = try await client.status()
    #expect(status.deviceName == "Test Mac")
    #expect(status.lanDiscovery)
}

@Test func authenticatedExportSendsBearerAndDecodesSettings() async throws {
    let token = String(repeating: "a", count: 32)
    let recorder = RequestRecorder { request in
        #expect(request.httpMethod == "POST")
        #expect(request.value(forHTTPHeaderField: "Authorization") == "Bearer \(token)")
        return TestResponse.response(
            request,
            status: 200,
            json: #"{"schemaVersion":1,"deviceName":"Home Mac","lanDiscoveryEnabled":false,"rememberedBackups":[]}"#
        )
    }
    let client = try makeClient(recorder: recorder, token: token)
    let settings = try await client.exportSettings()
    #expect(settings.deviceName == "Home Mac")
}

@Test func apiErrorsPreserveServerRecoveryMessage() async throws {
    let recorder = RequestRecorder { request in
        TestResponse.response(
            request,
            status: 409,
            json: #"{"protocolVersion":1,"code":"confirmation_required","message":"Explicit local confirmation is required.","retryable":false}"#
        )
    }
    let client = try makeClient(recorder: recorder, token: String(repeating: "b", count: 32))
    await #expect(throws: NodeClientError.api(
        status: 409,
        code: "confirmation_required",
        message: "Explicit local confirmation is required.",
        retryable: false
    )) {
        try await client.importSettings(
            ExportedDeviceSettings(deviceName: "Mac", lanDiscoveryEnabled: false, rememberedBackups: []),
            confirmed: false
        )
    }
}

@Test func bearerTokenIsNeverSentOverRemotePlainHTTP() async throws {
    let configuration = try NodeConnectionConfiguration(
        baseURL: URL(string: "http://192.0.2.10:8787")!,
        apiToken: String(repeating: "c", count: 32)
    )
    let client = NodeClient(configuration: configuration)
    await #expect(throws: NodeClientError.insecureAuthenticatedTransport) {
        _ = try await client.exportSettings()
    }
}

@Test func packagedCaddyTLSUsesEnrolledExactCAAndRejectsWrongCA() async throws {
    let environment = ProcessInfo.processInfo.environment
    guard let baseURLValue = environment["COVALENT_PACKAGE_TLS_BASE_URL"],
          let baseURL = URL(string: baseURLValue),
          let token = environment["COVALENT_PACKAGE_TLS_TOKEN"],
          let certificatePath = environment["COVALENT_PACKAGE_TLS_CERTIFICATE"],
          let wrongCertificatePath = environment["COVALENT_PACKAGE_TLS_WRONG_CERTIFICATE"]
    else {
        return
    }
    let certificate = try SecureNodeConnectionStore.parseCertificateFile(
        Data(contentsOf: URL(fileURLWithPath: certificatePath))
    )
    let defaultTrustClient = NodeClient(configuration: try NodeConnectionConfiguration(
        baseURL: baseURL,
        apiToken: token
    ))
    await #expect(throws: (any Error).self) {
        _ = try await defaultTrustClient.status()
    }

    let configuration = try NodeConnectionConfiguration(
        baseURL: baseURL,
        apiToken: token,
        trustedCertificateDER: certificate
    )
    let client = NodeClient(configuration: configuration)
    #expect(try await client.status().state == "ready")
    #expect(!(try await client.exportSettings()).deviceName.isEmpty)

    let wrongCertificate = try SecureNodeConnectionStore.parseCertificateFile(
        Data(contentsOf: URL(fileURLWithPath: wrongCertificatePath))
    )
    let rejected = NodeClient(configuration: try NodeConnectionConfiguration(
        baseURL: baseURL,
        apiToken: token,
        trustedCertificateDER: wrongCertificate
    ))
    await #expect(throws: (any Error).self) {
        _ = try await rejected.status()
    }

    let wrongTokenClient = NodeClient(configuration: try NodeConnectionConfiguration(
        baseURL: baseURL,
        apiToken: String(repeating: "x", count: 32),
        trustedCertificateDER: certificate
    ))
    await #expect(throws: NodeClientError.unauthorized) {
        _ = try await wrongTokenClient.exportSettings()
    }
}

@Test func resumableJobControlUsesAuthenticatedVersionedContract() async throws {
    let token = String(repeating: "d", count: 32)
    let recorder = RequestRecorder { request in
        #expect(request.url?.path == "/api/v1/jobs/control")
        #expect(request.httpMethod == "POST")
        #expect(request.value(forHTTPHeaderField: "Authorization") == "Bearer \(token)")
        let body = try #require(requestBody(request))
        let payload = try #require(JSONSerialization.jsonObject(with: body) as? [String: String])
        #expect(payload["jobId"] == "backup-menu-test")
        #expect(payload["action"] == "pause")
        return TestResponse.response(
            request,
            status: 200,
            json: #"{"jobId":"backup-menu-test","state":"paused"}"#
        )
    }
    let client = try makeClient(recorder: recorder, token: token)
    let response = try await client.controlJob(jobId: "backup-menu-test", action: .pause)
    #expect(response.state == .paused)
}

@Test func restorePreviewUsesDurableReferenceAndBoundedPages() async throws {
    let token = String(repeating: "e", count: 32)
    let sequence = RequestSequence()
    let planId = String(repeating: "c", count: 64)
    let planDigest = String(repeating: "b", count: 64)
    let manifestDigest = String(repeating: "a", count: 64)
    let backupId = "11111111-1111-1111-1111-111111111111"
    let signerId = "22222222-2222-2222-2222-222222222222"
    let reference = #"{"planId":"\#(planId)","planDigest":"\#(planDigest)","backupId":"\#(backupId)","snapshotId":"snapshot-test","authorizedRoot":"/private/stage","manifestDigest":"\#(manifestDigest)","conflictPolicy":"fail","jobId":"restore-test","signerDeviceId":"\#(signerId)","signature":"signed","totalEntries":2}"#
    let recorder = RequestRecorder(removeAfterRequest: false) { request in
        switch sequence.next() {
        case 0:
            #expect(request.url?.path == "/api/v1/restores/preview")
            #expect(request.httpMethod == "POST")
            return TestResponse.response(
                request,
                status: 200,
                json: reference,
                headers: [
                    "Cache-Control": "no-store",
                    AppleArchiveTransfer.restorePlanIdHeader: planId,
                    AppleArchiveTransfer.restorePlanDigestHeader: planDigest,
                ]
            )
        case 1:
            #expect(request.url?.path == "/api/v1/restores/plans/\(planId)")
            #expect(URLComponents(url: try #require(request.url), resolvingAgainstBaseURL: false)?.queryItems?.contains(URLQueryItem(name: "limit", value: "1000")) == true)
            return TestResponse.response(
                request,
                status: 200,
                json: #"{"planId":"\#(planId)","backupId":"\#(backupId)","snapshotId":"snapshot-test","authorizedRoot":"/private/stage","manifestDigest":"\#(manifestDigest)","conflictPolicy":"fail","jobId":"restore-test","planDigest":"\#(planDigest)","signerDeviceId":"\#(signerId)","signature":"signed","entryOffset":0,"totalEntries":2,"entries":[{"sourcePath":"folder","destinationPath":"folder","kind":"directory","action":"create_directory"}],"nextCursor":"1"}"#
            )
        case 2:
            #expect(URLComponents(url: try #require(request.url), resolvingAgainstBaseURL: false)?.queryItems?.contains(URLQueryItem(name: "cursor", value: "1")) == true)
            return TestResponse.response(
                request,
                status: 200,
                json: #"{"planId":"\#(planId)","backupId":"\#(backupId)","snapshotId":"snapshot-test","authorizedRoot":"/private/stage","manifestDigest":"\#(manifestDigest)","conflictPolicy":"fail","jobId":"restore-test","planDigest":"\#(planDigest)","signerDeviceId":"\#(signerId)","signature":"signed","entryOffset":1,"totalEntries":2,"entries":[{"sourcePath":"folder/file.txt","destinationPath":"folder/file.txt","kind":"file","action":"create_file"}],"nextCursor":null}"#
            )
        default:
            throw NodeClientError.invalidResponse
        }
    }
    let client = try makeClient(recorder: recorder, token: token)
    let plan = try await client.previewRestore(
        RestorePreviewRequest(
            backupId: UUID(uuidString: backupId)!,
            snapshotId: "snapshot-test",
            targetRoot: "/restore",
            conflictPolicy: .fail,
            jobId: "restore-test"
        )
    )
    #expect(plan.planId == planId)
    #expect(plan.entries.map(\.destinationPath) == ["folder", "folder/file.txt"])
    #expect(sequence.count == 3)
}

@Test func restoreExecuteSendsOnlyTheDurablePlanIdentifier() async throws {
    let plan = testRestorePlan()
    let recorder = RequestRecorder { request in
        let body = try #require(requestBody(request))
        let payload = try #require(JSONSerialization.jsonObject(with: body) as? [String: String])
        #expect(payload == ["planId": plan.planId])
        return TestResponse.response(
            request,
            status: 200,
            json: #"{"filesRestored":1,"directoriesCreated":1,"filesSkipped":0,"bytesWritten":12,"rejectedProviderCopies":0}"#,
            headers: [
                AppleArchiveTransfer.restorePlanIdHeader: plan.planId,
                AppleArchiveTransfer.restorePlanDigestHeader: plan.planDigest,
            ]
        )
    }
    let client = try makeClient(recorder: recorder, token: String(repeating: "f", count: 32))
    let result = try await client.executeRestore(plan)
    #expect(result.filesRestored == 1)
}

@Test func realDaemonBackupVerifyAndRestore() async throws {
    let environment = ProcessInfo.processInfo.environment
    guard let baseURLValue = environment["COVALENT_INTEGRATION_BASE_URL"],
          let baseURL = URL(string: baseURLValue),
          let token = environment["COVALENT_INTEGRATION_TOKEN"],
          let sourcePath = environment["COVALENT_INTEGRATION_SOURCE"],
          let restorePath = environment["COVALENT_INTEGRATION_RESTORE"]
    else {
        return
    }

    let source = URL(fileURLWithPath: sourcePath, isDirectory: true)
    let restore = URL(fileURLWithPath: restorePath, isDirectory: true)
    try FileManager.default.createDirectory(at: source.appending(path: "Documents"), withIntermediateDirectories: true)
    try Data("real daemon integration\n".utf8).write(to: source.appending(path: "Documents/notes.txt"))
    var state: UInt64 = 0xC0A1_E17A_5EED_1234
    var largeBytes = [UInt8](repeating: 0, count: 3 * 1_024 * 1_024)
    for index in largeBytes.indices {
        state = state &* 6_364_136_223_846_793_005 &+ 1_442_695_040_888_963_407
        largeBytes[index] = UInt8(truncatingIfNeeded: state >> 24)
    }
    let largePayload = Data(largeBytes)
    try largePayload.write(to: source.appending(path: "Documents/large.bin"))
    try FileManager.default.createDirectory(at: restore, withIntermediateDirectories: true)

    let configuration = try NodeConnectionConfiguration(baseURL: baseURL, apiToken: token)
    let client = NodeClient(configuration: configuration)
    let status = try await client.status()
    #expect(status.protocolVersion == covalentProtocolVersion)

    let initial = try await client.exportSettings()
    let updated = ExportedDeviceSettings(
        deviceName: "Apple Integration Node",
        lanDiscoveryEnabled: false,
        rememberedBackups: initial.rememberedBackups
    )
    try await client.importSettings(updated, confirmed: true)
    #expect(try await client.exportSettings().deviceName == "Apple Integration Node")

    let backupId = UUID()
    let snapshotId = "swift-\(UUID().uuidString.lowercased())"
    let backupJobId = "backup-\(UUID().uuidString.lowercased())"
    let backup = try await client.createBackupArchive(
        sourceURL: source,
        metadata: ArchiveBackupMetadata(
            backupId: backupId,
            displayName: "Swift integration",
            snapshotId: snapshotId,
            jobId: backupJobId,
            selectedProviderIds: []
        )
    )
    #expect(backup.backupId == backupId)
    #expect(backup.entries >= 3)
    #expect(backup.bytesRead > 2 * 1_024 * 1_024)
    #expect(backup.selectedProviders == 0)
    #expect(backup.degradedFailures == 0)
    try await client.acknowledgeJob(jobId: backupJobId)

    let verification = try await client.verifySnapshot(
        SnapshotRequest(backupId: backupId, snapshotId: snapshotId, verifyProviders: false, repair: false)
    )
    #expect(verification.intact)
    #expect(verification.missing.isEmpty)
    #expect(verification.corrupt.isEmpty)

    let restoreJobId = "restore-\(UUID().uuidString.lowercased())"
    let plan = try await client.previewArchiveRestore(
        backupId: backupId,
        snapshotId: snapshotId,
        conflictPolicy: .fail,
        jobId: restoreJobId
    )
    #expect(plan.entries.contains { $0.destinationPath.hasSuffix("Documents/notes.txt") })
    let result = try await client.executeArchiveRestore(plan, targetURL: restore)
    #expect(result.filesRestored == 2)
    #expect(result.rejectedProviderCopies == 0)
    let restored = try String(contentsOf: restore.appending(path: "Documents/notes.txt"), encoding: .utf8)
    #expect(restored == "real daemon integration\n")
    #expect(try Data(contentsOf: restore.appending(path: "Documents/large.bin")) == largePayload)
    try await client.acknowledgeJob(jobId: restoreJobId)
}

@Test func streamedRestoreRejectsNonEmptyDestinationBeforeNetworkExecution() async throws {
    let directory = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString, directoryHint: .isDirectory)
    defer { try? FileManager.default.removeItem(at: directory) }
    try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
    try Data("existing".utf8).write(to: directory.appending(path: "keep.txt"))
    #expect(throws: AppleArchiveTransferError.restoreDestinationMustBeEmpty) {
        try AppleArchiveTransfer.requireEmptyDirectory(directory)
    }
    #expect(try String(contentsOf: directory.appending(path: "keep.txt"), encoding: .utf8) == "existing")
}

@Test func archiveRestoreUsesDescriptorAnchoredNoFollowTraversal() throws {
    let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString, directoryHint: .isDirectory)
    let source = root.appending(path: "source", directoryHint: .isDirectory)
    let destination = root.appending(path: "destination", directoryHint: .isDirectory)
    defer { try? FileManager.default.removeItem(at: root) }
    try FileManager.default.createDirectory(at: source.appending(path: "nested"), withIntermediateDirectories: true)
    try Data("descriptor confined\n".utf8).write(to: source.appending(path: "nested/note.txt"))
    try FileManager.default.createDirectory(at: destination, withIntermediateDirectories: true)
    let archive = try AppleArchiveTransfer.makeBackupArchive(sourceURL: source)
    defer { try? FileManager.default.removeItem(at: archive) }

    try AppleArchiveTransfer.extractRestoreArchive(archive, to: destination, plan: testRestorePlan())
    #expect(try String(contentsOf: destination.appending(path: "nested/note.txt"), encoding: .utf8) == "descriptor confined\n")
}

@Test func archiveRestoreRejectsRootAndChildDirectorySwapRaces() throws {
    let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString, directoryHint: .isDirectory)
    let source = root.appending(path: "source", directoryHint: .isDirectory)
    let destination = root.appending(path: "destination", directoryHint: .isDirectory)
    let movedDestination = root.appending(path: "destination-moved", directoryHint: .isDirectory)
    let outside = root.appending(path: "outside", directoryHint: .isDirectory)
    defer { try? FileManager.default.removeItem(at: root) }
    try FileManager.default.createDirectory(at: source.appending(path: "nested"), withIntermediateDirectories: true)
    try Data("must stay confined\n".utf8).write(to: source.appending(path: "nested/note.txt"))
    try FileManager.default.createDirectory(at: destination, withIntermediateDirectories: true)
    try FileManager.default.createDirectory(at: outside, withIntermediateDirectories: true)
    let archive = try AppleArchiveTransfer.makeBackupArchive(sourceURL: source)
    defer { try? FileManager.default.removeItem(at: archive) }

    #expect(throws: AppleArchiveTransferError.destinationChanged) {
        try AppleArchiveTransfer.extractRestoreArchive(
            archive,
            to: destination,
            plan: testRestorePlan(),
            beforeWriting: {
                try FileManager.default.moveItem(at: destination, to: movedDestination)
                try FileManager.default.createDirectory(at: destination, withIntermediateDirectories: false)
            }
        )
    }
    #expect(try FileManager.default.contentsOfDirectory(atPath: destination.path).isEmpty)
    #expect(try FileManager.default.contentsOfDirectory(atPath: movedDestination.path).isEmpty)

    try FileManager.default.removeItem(at: destination)
    try FileManager.default.moveItem(at: movedDestination, to: destination)
    #expect(throws: AppleArchiveTransferError.destinationChanged) {
        try AppleArchiveTransfer.extractRestoreArchive(
            archive,
            to: destination,
            plan: testRestorePlan(),
            beforeEntry: { components in
                guard components == ["nested", "note.txt"] else { return }
                try FileManager.default.removeItem(at: destination.appending(path: "nested"))
                try FileManager.default.createSymbolicLink(
                    at: destination.appending(path: "nested"),
                    withDestinationURL: outside
                )
            }
        )
    }
    #expect(try FileManager.default.contentsOfDirectory(atPath: outside.path).isEmpty)
}

private func testRestorePlan() -> RestorePlan {
    RestorePlan(
        reference: RestorePlanReference(
            planId: String(repeating: "c", count: 64),
            backupId: UUID(),
            snapshotId: "snapshot-test",
            authorizedRoot: "/",
            manifestDigest: String(repeating: "a", count: 64),
            conflictPolicy: .fail,
            jobId: "restore-test",
            planDigest: String(repeating: "b", count: 64),
            signerDeviceId: UUID(),
            signature: "test",
            totalEntries: 2
        ),
        entries: [
            RestorePreviewEntry(
                sourcePath: "nested",
                destinationPath: "nested",
                kind: .directory,
                action: .createDirectory
            ),
            RestorePreviewEntry(
                sourcePath: "nested/note.txt",
                destinationPath: "nested/note.txt",
                kind: .file,
                action: .createFile
            ),
        ]
    )
}

private func makeClient(recorder: RequestRecorder, token: String?) throws -> NodeClient {
    let sessionConfiguration = URLSessionConfiguration.ephemeral
    sessionConfiguration.protocolClasses = [RecordingURLProtocol.self]
    let port = RecordingURLProtocol.recorder.install(recorder)
    let session = URLSession(configuration: sessionConfiguration)
    let configuration = try NodeConnectionConfiguration(
        baseURL: URL(string: "http://127.0.0.1:\(port)")!,
        apiToken: token
    )
    return NodeClient(configuration: configuration, session: session)
}

private func requestBody(_ request: URLRequest) -> Data? {
    if let body = request.httpBody { return body }
    guard let stream = request.httpBodyStream else { return nil }
    stream.open()
    defer { stream.close() }
    var data = Data()
    let buffer = UnsafeMutablePointer<UInt8>.allocate(capacity: 4_096)
    defer { buffer.deallocate() }
    while stream.hasBytesAvailable {
        let count = stream.read(buffer, maxLength: 4_096)
        guard count >= 0 else { return nil }
        if count == 0 { break }
        data.append(buffer, count: count)
    }
    return data
}

private struct RequestRecorder: @unchecked Sendable {
    let removeAfterRequest: Bool
    let handler: (URLRequest) throws -> (HTTPURLResponse, Data)

    init(
        removeAfterRequest: Bool = true,
        handler: @escaping (URLRequest) throws -> (HTTPURLResponse, Data)
    ) {
        self.removeAfterRequest = removeAfterRequest
        self.handler = handler
    }
}

private final class RequestSequence: @unchecked Sendable {
    private let lock = NSLock()
    private var value = 0

    var count: Int {
        lock.withLock { value }
    }

    func next() -> Int {
        lock.withLock {
            defer { value += 1 }
            return value
        }
    }
}

private final class RecorderBox: @unchecked Sendable {
    private let lock = NSLock()
    private var values: [Int: RequestRecorder] = [:]
    private var nextPort = 20_000

    func install(_ recorder: RequestRecorder) -> Int {
        lock.lock()
        defer { lock.unlock() }
        nextPort += 1
        values[nextPort] = recorder
        return nextPort
    }

    func current(for request: URLRequest) -> RequestRecorder? {
        lock.lock()
        defer { lock.unlock() }
        guard let port = request.url?.port else { return nil }
        return values[port]
    }

    func remove(for request: URLRequest) {
        lock.lock()
        if let port = request.url?.port {
            values.removeValue(forKey: port)
        }
        lock.unlock()
    }
}

private final class RecordingURLProtocol: URLProtocol, @unchecked Sendable {
    static let recorder = RecorderBox()

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        guard let recorder = Self.recorder.current(for: request) else {
            client?.urlProtocol(self, didFailWithError: NodeClientError.invalidResponse)
            return
        }
        do {
            let (response, data) = try recorder.handler(request)
            client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            client?.urlProtocol(self, didLoad: data)
            client?.urlProtocolDidFinishLoading(self)
            if recorder.removeAfterRequest { Self.recorder.remove(for: request) }
        } catch {
            client?.urlProtocol(self, didFailWithError: error)
            if recorder.removeAfterRequest { Self.recorder.remove(for: request) }
        }
    }

    override func stopLoading() {}
}

private enum TestResponse {
    static func response(
        _ request: URLRequest,
        status: Int,
        json: String,
        headers: [String: String] = [:]
    ) -> (HTTPURLResponse, Data) {
        let response = HTTPURLResponse(
            url: request.url!,
            statusCode: status,
            httpVersion: "HTTP/1.1",
            headerFields: ["Content-Type": "application/json"].merging(headers) { _, new in new }
        )!
        return (response, Data(json.utf8))
    }
}
