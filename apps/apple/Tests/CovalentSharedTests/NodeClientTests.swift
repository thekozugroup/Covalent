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
    let largePayload = Data(repeating: 0x5A, count: 3 * 1_024 * 1_024)
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
    let backup = try await client.createBackupArchive(
        sourceURL: source,
        metadata: ArchiveBackupMetadata(
            backupId: backupId,
            displayName: "Swift integration",
            snapshotId: snapshotId,
            jobId: "backup-\(UUID().uuidString.lowercased())",
            selectedProviderIds: []
        )
    )
    #expect(backup.backupId == backupId)
    #expect(backup.entries >= 3)
    #expect(backup.bytesRead > 2 * 1_024 * 1_024)
    #expect(backup.selectedProviders == 0)
    #expect(backup.degradedFailures == 0)

    let verification = try await client.verifySnapshot(
        SnapshotRequest(backupId: backupId, snapshotId: snapshotId, verifyProviders: false, repair: false)
    )
    #expect(verification.intact)
    #expect(verification.missing.isEmpty)
    #expect(verification.corrupt.isEmpty)

    let plan = try await client.previewArchiveRestore(
        backupId: backupId,
        snapshotId: snapshotId,
        conflictPolicy: .fail,
        jobId: "restore-\(UUID().uuidString.lowercased())"
    )
    #expect(plan.entries.contains { $0.destinationPath.hasSuffix("Documents/notes.txt") })
    let result = try await client.executeArchiveRestore(plan, targetURL: restore)
    #expect(result.filesRestored == 2)
    #expect(result.rejectedProviderCopies == 0)
    let restored = try String(contentsOf: restore.appending(path: "Documents/notes.txt"), encoding: .utf8)
    #expect(restored == "real daemon integration\n")
    #expect(try Data(contentsOf: restore.appending(path: "Documents/large.bin")) == largePayload)
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
    let handler: (URLRequest) throws -> (HTTPURLResponse, Data)
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
            Self.recorder.remove(for: request)
        } catch {
            client?.urlProtocol(self, didFailWithError: error)
            Self.recorder.remove(for: request)
        }
    }

    override func stopLoading() {}
}

private enum TestResponse {
    static func response(_ request: URLRequest, status: Int, json: String) -> (HTTPURLResponse, Data) {
        let response = HTTPURLResponse(
            url: request.url!,
            statusCode: status,
            httpVersion: "HTTP/1.1",
            headerFields: ["Content-Type": "application/json"]
        )!
        return (response, Data(json.utf8))
    }
}
