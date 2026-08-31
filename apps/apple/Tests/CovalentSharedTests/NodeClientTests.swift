import Darwin
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

@Test func networkPairingUsesOneStepSASContract() async throws {
    let token = String(repeating: "9", count: 32)
    let peerId = UUID()
    let pairingId = "pairing_network_test"
    let fingerprint = String(repeating: "a", count: 64)
    let sequence = RequestSequence()
    let recorder = RequestRecorder(removeAfterRequest: false) { request in
        switch sequence.next() {
        case 0:
            #expect(request.url?.path == "/api/v1/pair/network/start")
            let body = try #require(requestBody(request))
            let payload = try #require(JSONSerialization.jsonObject(with: body) as? [String: String])
            #expect(payload["candidateAddress"] == "nas.example.ts.net:8788")
            return TestResponse.response(
                request,
                status: 200,
                json: #"{"pairingId":"\#(pairingId)","direction":"outgoing","peerName":"NAS","authenticationString":"1234-5678-9012-3456","expiresAtUnixMs":9999999999999,"state":"awaiting_local_confirmation","failureCode":null,"failureMessage":null,"peerTransport":null}"#
            )
        case 1:
            #expect(request.url?.path == "/api/v1/pair/network/\(pairingId)/confirm")
            let body = try #require(requestBody(request))
            let payload = try #require(JSONSerialization.jsonObject(with: body) as? [String: String])
            #expect(payload["displayedCode"] == "1234-5678-9012-3456")
            return TestResponse.response(
                request,
                status: 200,
                json: #"{"pairingId":"\#(pairingId)","direction":"outgoing","peerName":"NAS","authenticationString":"1234-5678-9012-3456","expiresAtUnixMs":9999999999999,"state":"complete","failureCode":null,"failureMessage":null,"peerTransport":{"peerId":"\#(peerId.uuidString.lowercased())","displayName":"NAS","address":"100.100.100.10:8788","certificateDer":"signed-der","certificateFingerprint":"\#(fingerprint)"}}"#
            )
        default:
            Issue.record("Unexpected network-pairing request")
            return TestResponse.response(request, status: 500, json: "{}")
        }
    }
    let client = try makeClient(recorder: recorder, token: token)
    let started = try await client.startNetworkPairing(candidateAddress: "nas.example.ts.net:8788")
    #expect(started.state == .awaitingLocalConfirmation)
    let completed = try await client.confirmNetworkPairing(
        pairingId: started.id,
        displayedCode: started.authenticationString
    )
    #expect(completed.state == .complete)
    #expect(completed.peerTransport?.certificateFingerprint == fingerprint)
}

@Test func signedSHA256ProviderPinIsAcceptedAndPersistedExactly() async throws {
    let token = String(repeating: "8", count: 32)
    let peerId = UUID()
    let fingerprint = String(repeating: "b", count: 64)
    let transport = PeerTransport(
        peerId: peerId,
        displayName: "Backup NAS",
        address: "100.100.100.11:8788",
        certificateDer: "signed-der",
        certificateFingerprint: fingerprint
    )
    let providerJSON = #"{"peerId":"\#(peerId.uuidString.lowercased())","address":"100.100.100.11:8788","certificateFingerprint":"\#(fingerprint)","reachability":"reachable","observedAtUnixMs":1,"validUntilUnixMs":2,"usableBytes":1024,"allocatedBytes":2048,"quotaBytes":4096}"#
    let sequence = RequestSequence()
    let recorder = RequestRecorder(removeAfterRequest: false) { request in
        switch sequence.next() {
        case 0:
            #expect(request.url?.path == "/api/v1/providers/connect")
            let body = try #require(requestBody(request))
            let payload = try #require(JSONSerialization.jsonObject(with: body) as? [String: Any])
            let signed = try #require(payload["peerTransport"] as? [String: String])
            #expect(payload["peerId"] == nil)
            #expect(signed["peerId"]?.lowercased() == peerId.uuidString.lowercased())
            #expect(signed["certificateFingerprint"] == fingerprint)
            return TestResponse.response(request, status: 200, json: providerJSON)
        case 1:
            #expect(request.url?.path == "/api/v1/providers")
            return TestResponse.response(request, status: 200, json: "[\(providerJSON)]")
        default:
            Issue.record("Unexpected provider request")
            return TestResponse.response(request, status: 500, json: "{}")
        }
    }
    let client = try makeClient(recorder: recorder, token: token)
    let connected = try await client.connectProvider(using: transport)
    try CovalentAppModel.validateProviderBinding(connected, transport: transport)
    #expect(connected.capacity?.usableBytes == 1_024)
    #expect(connected.capacity?.allocatedBytes == 2_048)
    #expect(connected.capacity?.quotaBytes == 4_096)
    let persisted = try await client.providers()
    #expect(persisted.contains { $0.peerId == peerId && $0.certificateFingerprint == fingerprint })
}

@Test func signedProviderPinMismatchIsRejected() throws {
    let peerId = UUID()
    let transport = PeerTransport(
        peerId: peerId,
        displayName: "Backup NAS",
        address: "100.100.100.12:8788",
        certificateDer: "signed-der",
        certificateFingerprint: String(repeating: "c", count: 64)
    )
    let wrong = ProviderConnection(
        peerId: peerId,
        address: transport.address,
        certificateFingerprint: String(repeating: "d", count: 64)
    )
    #expect(throws: AppModelError.providerBindingMismatch) {
        try CovalentAppModel.validateProviderBinding(wrong, transport: transport)
    }
}

@Test func providerSelectionRequiresCurrentReachableNonzeroCapacity() {
    let now = UInt64(Date().timeIntervalSince1970 * 1_000)
    let base = ProviderConnection(
        peerId: UUID(),
        address: "100.100.100.12:8788",
        certificateFingerprint: String(repeating: "c", count: 64)
    )
    #expect(!base.isEligibleForBackup)
    #expect(!ProviderConnection(
        peerId: base.peerId,
        address: base.address,
        certificateFingerprint: base.certificateFingerprint,
        reachability: .reachable,
        observedAtUnixMs: now - 10_000,
        validUntilUnixMs: now - 5_000,
        capacity: ProviderCapacity(usableBytes: 1, allocatedBytes: 1, quotaBytes: 2)
    ).isEligibleForBackup)
    #expect(ProviderConnection(
        peerId: base.peerId,
        address: base.address,
        certificateFingerprint: base.certificateFingerprint,
        reachability: .reachable,
        observedAtUnixMs: now,
        validUntilUnixMs: now + 5_000,
        capacity: ProviderCapacity(usableBytes: 1, allocatedBytes: 1, quotaBytes: 2)
    ).isEligibleForBackup)
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

/// Drives a real packaged Caddy over TLS. Requires the four
/// `COVALENT_PACKAGE_TLS_*` location variables, which `scripts/apple-package-tls-e2e.sh`
/// supplies.
///
/// The trait is what makes the absence of that environment *visible*. This
/// test used to open with `guard … else { return }`, and a bare `return` in
/// swift-testing is a **pass**, not a skip: with nothing configured it
/// reported "1 test passed" without opening a socket, and had reported that
/// for its whole life, because nothing has ever set `COVALENT_RUN_APPLE_TLS_E2E`
/// (its only occurrence outside the guard is prose in
/// `packaging/docker/README.md`). Now it reports as skipped and says so.
///
/// The pinning property this covers against a real server is *also* covered
/// unconditionally, without any infrastructure, by `PinnedTrustTests` — so a
/// skip here is a loss of end-to-end confidence, not a hole in the contract.
@Test(
    .enabled(
        if: ProcessInfo.processInfo.environment["COVALENT_PACKAGE_TLS_BASE_URL"] != nil,
        "COVALENT_PACKAGE_TLS_BASE_URL is unset, so no packaged server is running to test against"
    )
)
func packagedCaddyTLSUsesEnrolledExactCAAndRejectsWrongCA() async throws {
    let environment = ProcessInfo.processInfo.environment
    // Everything past the trait is `#require`, not `guard … else { return }`.
    // A driver that sets the base URL but forgets a certificate path is a
    // broken driver, and this must fail rather than quietly report a pass.
    let baseURLValue = try #require(environment["COVALENT_PACKAGE_TLS_BASE_URL"])
    let baseURL = try #require(URL(string: baseURLValue))
    let tokenFile = try #require(environment["COVALENT_PACKAGE_TLS_TOKEN_FILE"])
    let token = try readOwnerOnlyTestToken(at: tokenFile)
    let certificatePath = try #require(environment["COVALENT_PACKAGE_TLS_CERTIFICATE"])
    let wrongCertificatePath = try #require(environment["COVALENT_PACKAGE_TLS_WRONG_CERTIFICATE"])
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

/// Drives a real `covalent-node` process end to end.
/// `Scripts/integration-test.sh` builds the node, starts it, and supplies the
/// integration location variables plus a mode-0600 test token file.
///
/// The trait replaces a `guard … else { return }`. A bare `return` in
/// swift-testing is a **pass**: in a plain `swift test` run this reported
/// "passed" having never spoken to a daemon, so the suite's headline count
/// included a test that had done nothing. It now reports as skipped, and
/// `Scripts/integration-test.sh` fails if it ever reports as skipped there.
@Test(
    .enabled(
        if: ProcessInfo.processInfo.environment["COVALENT_INTEGRATION_BASE_URL"] != nil,
        "COVALENT_INTEGRATION_BASE_URL is unset, so no covalent-node is running to test against"
    )
)
func realDaemonBackupVerifyAndRestore() async throws {
    let environment = ProcessInfo.processInfo.environment
    // `#require`, not `guard … else { return }`: a driver that sets the base
    // URL and forgets the source directory must fail, not report a pass.
    let baseURLValue = try #require(environment["COVALENT_INTEGRATION_BASE_URL"])
    let baseURL = try #require(URL(string: baseURLValue))
    let tokenFile = try #require(environment["COVALENT_INTEGRATION_TOKEN_FILE"])
    let token = try readOwnerOnlyTestToken(at: tokenFile)
    let sourcePath = try #require(environment["COVALENT_INTEGRATION_SOURCE"])
    let restorePath = try #require(environment["COVALENT_INTEGRATION_RESTORE"])

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
        jobId: restoreJobId,
        targetURL: restore
    )
    #expect(plan.entries.contains { $0.destinationPath.hasSuffix("Documents/notes.txt") })
    // Drive the restore with a progress sink so the task-progress delegate is
    // exercised against the real daemon over real CFNetwork. Installing a
    // delegate must not disturb the async download's file handoff — if it
    // did, the restored bytes below would not match.
    let restoreProgress = RestoreProgressLog()
    let result = try await client.executeArchiveRestore(
        plan,
        targetURL: restore,
        onProgress: { restoreProgress.record($0) }
    )
    #expect(result.filesRestored == 2)
    let restorePhases = restoreProgress.snapshots.map(\.phase)
    #expect(restorePhases.first == .preparing)
    #expect(restorePhases.last == .finishing)
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

@Test func downloadedArchiveAdoptionAvoidsASecondDiskCopyAndHashesIncrementally() throws {
    let source = FileManager.default.temporaryDirectory.appending(path: "download-\(UUID().uuidString).zip")
    try Data("abc".utf8).write(to: source)
    var before = stat()
    #expect(source.path.withCString { lstat($0, &before) } == 0)

    let adopted = try AppleArchiveTransfer.copyDownloadedArchive(source)
    defer { try? FileManager.default.removeItem(at: adopted) }
    var after = stat()
    #expect(adopted.path.withCString { lstat($0, &after) } == 0)
    #expect(before.st_dev == after.st_dev)
    #expect(before.st_ino == after.st_ino)
    #expect(!FileManager.default.fileExists(atPath: source.path))

    let identity = try AppleArchiveTransfer.uploadIdentity(for: adopted)
    #expect(identity.length == 3)
    #expect(identity.digest == "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
}

@Test func backupArchiveRejectsAFileSwappedToSymlinkBeforeDescriptorOpen() throws {
    let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString, directoryHint: .isDirectory)
    let source = root.appending(path: "source", directoryHint: .isDirectory)
    let outside = root.appending(path: "outside.txt")
    let victim = source.appending(path: "victim.txt")
    defer { try? FileManager.default.removeItem(at: root) }
    try FileManager.default.createDirectory(at: source, withIntermediateDirectories: true)
    try Data("private".utf8).write(to: victim)
    try Data("outside".utf8).write(to: outside)

    #expect(throws: AppleArchiveTransferError.sourceChanged("victim.txt")) {
        _ = try AppleArchiveTransfer.makeBackupArchive(sourceURL: source) { components in
            if components == ["victim.txt"] {
                try FileManager.default.removeItem(at: victim)
                try FileManager.default.createSymbolicLink(at: victim, withDestinationURL: outside)
            }
        }
    }
}

@Test func archiveUploadResumesOnlyTheAuthoritativeSuffixAfterProcessRelaunch() async throws {
    let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString, directoryHint: .isDirectory)
    let source = root.appending(path: "source", directoryHint: .isDirectory)
    let transfers = root.appending(path: "transfers", directoryHint: .isDirectory)
    defer { try? FileManager.default.removeItem(at: root) }
    try FileManager.default.createDirectory(at: source, withIntermediateDirectories: true)
    var payload = Data(count: 512 * 1_024)
    payload.withUnsafeMutableBytes { bytes in
        guard let values = bytes.bindMemory(to: UInt8.self).baseAddress else { return }
        for index in 0..<bytes.count { values[index] = UInt8(truncatingIfNeeded: index &* 31 &+ 7) }
    }
    try payload.write(to: source.appending(path: "payload.bin"))
    let metadata = ArchiveBackupMetadata(
        displayName: "Resume proof",
        snapshotId: "resume-1",
        jobId: "apple-resume-relaunch",
        selectedProviderIds: []
    )
    let token = String(repeating: "r", count: 40)
    let interrupted = RequestRecorder { request in
        #expect(request.url?.path == "/api/v1/backups/archive")
        #expect(request.value(forHTTPHeaderField: AppleArchiveTransfer.uploadOffsetHeader) == "0")
        _ = try #require(requestBody(request))
        throw URLError(.networkConnectionLost)
    }
    let first = try makeClient(recorder: interrupted, token: token, transferDirectory: transfers)
    do {
        _ = try await first.createBackupArchive(sourceURL: source, metadata: metadata)
        Issue.record("Interrupted upload unexpectedly succeeded")
    } catch {
        // Durable state must survive this actor/session just as it survives process termination.
    }
    let archiveURL = try #require(
        FileManager.default.contentsOfDirectory(at: transfers, includingPropertiesForKeys: nil)
            .first { $0.pathExtension == "zip" }
    )
    let archiveBytes = try Data(contentsOf: archiveURL)
    let split = archiveBytes.count / 3
    let sequence = RequestSequence()
    let resumed = RequestRecorder(removeAfterRequest: false) { request in
        switch sequence.next() {
        case 0:
            #expect(request.url?.path == "/api/v1/backups/archive")
            #expect(request.value(forHTTPHeaderField: AppleArchiveTransfer.uploadOffsetHeader) == "0")
            #expect(requestBody(request) == archiveBytes)
            return TestResponse.response(
                request,
                status: 409,
                json: #"{"protocolVersion":1,"code":"upload_offset_mismatch","message":"resume","retryable":true}"#,
                headers: [AppleArchiveTransfer.uploadOffsetHeader: String(split)]
            )
        case 1:
            #expect(request.value(forHTTPHeaderField: AppleArchiveTransfer.uploadOffsetHeader) == String(split))
            #expect(requestBody(request) == archiveBytes.suffix(from: split))
            return TestResponse.response(
                request,
                status: 200,
                json: #"{"backupId":"11111111-2222-4333-8444-555555555555","snapshotId":"resume-1","entries":1,"bytesRead":524288,"chunksStored":1,"chunksDeduplicated":0,"selectedProviders":0,"degradedFailures":0}"#,
                headers: [AppleArchiveTransfer.jobAcknowledgementRequiredHeader: "true"]
            )
        case 2:
            #expect(request.url?.path == "/api/v1/jobs/acknowledge")
            return TestResponse.response(request, status: 204, json: "")
        default:
            Issue.record("Unexpected resumed upload request")
            return TestResponse.response(request, status: 500, json: "{}")
        }
    }
    let second = try makeClient(recorder: resumed, token: token, transferDirectory: transfers)
    let response = try await second.createBackupArchive(sourceURL: source, metadata: metadata)
    #expect(response.snapshotId == "resume-1")
    #expect(sequence.count == 2)
    #expect(try FileManager.default.contentsOfDirectory(atPath: transfers.path).count == 2)
    try await second.acknowledgeJob(jobId: metadata.jobId)
    #expect(sequence.count == 3)
    #expect(try FileManager.default.contentsOfDirectory(atPath: transfers.path).isEmpty)
}

@Test func acceptedArchiveResultSurvivesRelaunchAndFailedAcknowledgement() async throws {
    let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString, directoryHint: .isDirectory)
    let source = root.appending(path: "source", directoryHint: .isDirectory)
    let transfers = root.appending(path: "transfers", directoryHint: .isDirectory)
    defer { try? FileManager.default.removeItem(at: root) }
    try FileManager.default.createDirectory(at: source, withIntermediateDirectories: true)
    try Data("durable accepted response".utf8).write(to: source.appending(path: "note.txt"))
    let metadata = ArchiveBackupMetadata(
        displayName: "Durable response",
        snapshotId: "accepted-1",
        jobId: "apple-accepted-response",
        selectedProviderIds: []
    )
    let sourceGrantId = UUID()
    let token = String(repeating: "a", count: 40)
    let firstRequests = RequestSequence()
    let firstRecorder = RequestRecorder(removeAfterRequest: false) { request in
        #expect(firstRequests.next() == 0)
        #expect(request.url?.path == "/api/v1/backups/archive")
        return TestResponse.response(
            request,
            status: 200,
            json: backupResponseJSON(backupId: UUID(), snapshotId: metadata.snapshotId),
            headers: [AppleArchiveTransfer.jobAcknowledgementRequiredHeader: "true"]
        )
    }
    let first = try makeClient(recorder: firstRecorder, token: token, transferDirectory: transfers)
    let accepted = try await first.createBackupArchive(
        sourceURL: source,
        metadata: metadata,
        sourceGrantId: sourceGrantId
    )
    let afterResponse = try await first.pendingArchiveBackups()
    #expect(afterResponse.count == 1)
    #expect(afterResponse[0].acceptedResponse == accepted)
    #expect(afterResponse[0].metadata == metadata)
    #expect(afterResponse[0].sourceGrantId == sourceGrantId)
    let durableRecord = try #require(
        FileManager.default.contentsOfDirectory(at: transfers, includingPropertiesForKeys: nil)
            .first { $0.pathExtension == "json" }
    )
    let durableArchive = try #require(
        FileManager.default.contentsOfDirectory(at: transfers, includingPropertiesForKeys: nil)
            .first { $0.pathExtension == "zip" }
    )
    let durableArchiveBytes = try Data(contentsOf: durableArchive)
    let durableText = String(decoding: try Data(contentsOf: durableRecord), as: UTF8.self)
    #expect(!durableText.contains(token))
    #expect(!durableText.localizedCaseInsensitiveContains("authorization"))
    #expect(!durableText.localizedCaseInsensitiveContains("bearer"))

    let acknowledgements = RequestSequence()
    let relaunchedRecorder = RequestRecorder(removeAfterRequest: false) { request in
        #expect(request.url?.path == "/api/v1/jobs/acknowledge")
        switch acknowledgements.next() {
        case 0:
            return TestResponse.response(
                request,
                status: 503,
                json: #"{"protocolVersion":1,"code":"temporarily_unavailable","message":"retry","retryable":true}"#
            )
        default:
            return TestResponse.response(request, status: 204, json: "")
        }
    }
    let relaunched = try makeClient(recorder: relaunchedRecorder, token: token, transferDirectory: transfers)
    // A saved accepted result is returned without uploading the archive again.
    #expect(try await relaunched.resumePendingArchiveBackup(jobId: metadata.jobId) == accepted)
    await #expect(throws: NodeClientError.self) {
        try await relaunched.acknowledgeJob(jobId: metadata.jobId)
    }
    #expect(try await relaunched.pendingArchiveBackups().count == 1)
    try await relaunched.acknowledgeJob(jobId: metadata.jobId)
    #expect(try await relaunched.pendingArchiveBackups().isEmpty)
    // Simulate a process exit after the acknowledged record was retired but
    // before its exact private archive could be unlinked.
    try durableArchiveBytes.write(to: durableArchive)
    try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: durableArchive.path)
    #expect(try await relaunched.pendingArchiveBackups().isEmpty)
    #expect(!FileManager.default.fileExists(atPath: durableArchive.path))
    // Server acknowledgement is idempotent even after local cleanup.
    try await relaunched.acknowledgeJob(jobId: metadata.jobId)
    #expect(acknowledgements.count == 3)
}

@Test func archiveJobMetadataMismatchFailsClosedBeforeNetworkRetry() async throws {
    let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString, directoryHint: .isDirectory)
    let source = root.appending(path: "source", directoryHint: .isDirectory)
    let transfers = root.appending(path: "transfers", directoryHint: .isDirectory)
    defer { try? FileManager.default.removeItem(at: root) }
    try FileManager.default.createDirectory(at: source, withIntermediateDirectories: true)
    try Data("same staged bytes".utf8).write(to: source.appending(path: "note.txt"))
    let original = ArchiveBackupMetadata(
        displayName: "Original",
        snapshotId: "metadata-1",
        jobId: "apple-metadata-binding",
        selectedProviderIds: []
    )
    let token = String(repeating: "m", count: 40)
    let interrupted = RequestRecorder { request in
        _ = try #require(requestBody(request))
        throw URLError(.networkConnectionLost)
    }
    let first = try makeClient(recorder: interrupted, token: token, transferDirectory: transfers)
    await #expect(throws: NodeClientError.self) {
        _ = try await first.createBackupArchive(sourceURL: source, metadata: original)
    }
    let unexpectedNetwork = RequestRecorder { request in
        Issue.record("Metadata mismatch reached the network: \(request.url?.path ?? "missing")")
        return TestResponse.response(request, status: 500, json: "{}")
    }
    let second = try makeClient(recorder: unexpectedNetwork, token: token, transferDirectory: transfers)
    let changed = ArchiveBackupMetadata(
        displayName: "Changed",
        snapshotId: original.snapshotId,
        jobId: original.jobId,
        selectedProviderIds: []
    )
    await #expect(throws: NodeClientError.self) {
        _ = try await second.createBackupArchive(sourceURL: source, metadata: changed)
    }
    #expect(try await second.pendingArchiveBackups().first?.metadata == original)
    let differentServerContext = try makeClient(
        recorder: unexpectedNetwork,
        token: String(repeating: "n", count: 40),
        transferDirectory: transfers
    )
    await #expect(throws: NodeClientError.self) {
        _ = try await differentServerContext.pendingArchiveBackups()
    }
}

@Test func moreThanEightAcceptedAppleJobsAreDiscoverableAndAcknowledgedWithoutEviction() async throws {
    let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString, directoryHint: .isDirectory)
    let source = root.appending(path: "source", directoryHint: .isDirectory)
    let transfers = root.appending(path: "transfers", directoryHint: .isDirectory)
    defer { try? FileManager.default.removeItem(at: root) }
    try FileManager.default.createDirectory(at: source, withIntermediateDirectories: true)
    try Data("receipt pressure".utf8).write(to: source.appending(path: "note.txt"))
    let token = String(repeating: "p", count: 40)
    let recorder = RequestRecorder(removeAfterRequest: false) { request in
        switch request.url?.path {
        case "/api/v1/backups/archive":
            let encoded = try #require(
                request.value(forHTTPHeaderField: AppleArchiveTransfer.metadataHeader)
            )
            let bytes = try #require(decodeBase64URL(encoded))
            let metadata = try JSONDecoder().decode(ArchiveBackupMetadata.self, from: bytes)
            return TestResponse.response(
                request,
                status: 200,
                json: backupResponseJSON(backupId: UUID(), snapshotId: metadata.snapshotId),
                headers: [AppleArchiveTransfer.jobAcknowledgementRequiredHeader: "true"]
            )
        case "/api/v1/jobs/acknowledge":
            return TestResponse.response(request, status: 204, json: "")
        default:
            Issue.record("Unexpected receipt-pressure request")
            return TestResponse.response(request, status: 500, json: "{}")
        }
    }
    let client = try makeClient(recorder: recorder, token: token, transferDirectory: transfers)
    for index in 0..<12 {
        let metadata = ArchiveBackupMetadata(
            displayName: "Interrupted \(index)",
            snapshotId: "pressure-\(index)",
            jobId: "apple-pressure-\(index)",
            selectedProviderIds: []
        )
        _ = try await client.createBackupArchive(sourceURL: source, metadata: metadata)
    }
    let discovered = try await client.pendingArchiveBackups()
    #expect(discovered.count == 12)
    #expect(Set(discovered.map { $0.metadata.jobId }).count == 12)
    for item in discovered {
        try await client.acknowledgeJob(jobId: item.metadata.jobId)
    }
    #expect(try await client.pendingArchiveBackups().isEmpty)
}

@Test func archiveRestoreJournalSurvivesRequestAndDownloadInterruptionsAcrossProcessRecreation() async throws {
    for checkpoint in [RestoreDurabilityCheckpoint.request, .download] {
        let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString, directoryHint: .isDirectory)
        defer { try? FileManager.default.removeItem(at: root) }
        let fixture = try makeRestoreJournalFixture(root: root, jobId: "restore-\(checkpoint.rawValue)")
        let token = String(repeating: "r", count: 40)
        let log = RestoreJournalRequestLog()
        let recorder = durableRestoreRecorder(plans: [fixture.plan.planId: fixture.plan], archive: fixture.archiveBytes, log: log)
        let port = RecordingURLProtocol.recorder.install(recorder)
        let interrupted = try makeClient(
            recorder: recorder,
            token: token,
            transferDirectory: fixture.transfers,
            restoreDurabilityFailpoint: { point in
                if point == checkpoint { throw RestoreJournalInterruption.interrupted }
            },
            connectionPort: port
        )
        await #expect(throws: RestoreJournalInterruption.self) {
            _ = try await interrupted.executeArchiveRestore(
                fixture.plan,
                targetURL: fixture.destination,
                destinationGrantId: fixture.destinationGrantId
            )
        }
        let journal = try #require(
            FileManager.default.contentsOfDirectory(at: fixture.transfers, includingPropertiesForKeys: nil)
                .first { $0.pathExtension == "json" }
        )
        let text = String(decoding: try Data(contentsOf: journal), as: UTF8.self)
        #expect(!text.contains(token))
        #expect(!text.localizedCaseInsensitiveContains("bearer"))
        let relaunched = try makeClient(
            recorder: recorder,
            token: token,
            transferDirectory: fixture.transfers,
            connectionPort: port
        )
        let response = try await relaunched.resumePendingArchiveRestore(
            jobId: fixture.plan.jobId,
            targetURL: fixture.destination,
            destinationGrantId: fixture.destinationGrantId
        )
        #expect(response == fixture.result)
        #expect(try String(contentsOf: fixture.destination.appending(path: "nested/note.txt"), encoding: .utf8) == "durable restore\n")
        try await relaunched.acknowledgeJob(jobId: fixture.plan.jobId)
        #expect(try await relaunched.pendingArchiveRestores().isEmpty)
        #expect(log.executeRequests == 1)
    }
}

@Test @MainActor func archiveRestoreJournalFailsClosedAfterAnInterruptedApplyAndPreviewAgainDiscardsIt() async throws {
    let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString, directoryHint: .isDirectory)
    defer { try? FileManager.default.removeItem(at: root) }
    let fixture = try makeRestoreJournalFixture(root: root, jobId: "restore-extraction")
    let token = String(repeating: "x", count: 40)
    let log = RestoreJournalRequestLog()
    let recorder = durableRestoreRecorder(plans: [fixture.plan.planId: fixture.plan], archive: fixture.archiveBytes, log: log)
    let port = RecordingURLProtocol.recorder.install(recorder)
    let interrupted = try makeClient(
        recorder: recorder,
        token: token,
        transferDirectory: fixture.transfers,
        restoreDurabilityFailpoint: { point in
            if point == .extraction { throw RestoreJournalInterruption.interrupted }
        },
        connectionPort: port
    )
    await #expect(throws: RestoreJournalInterruption.self) {
        _ = try await interrupted.executeArchiveRestore(
            fixture.plan,
            targetURL: fixture.destination,
            destinationGrantId: fixture.destinationGrantId
        )
    }
    let relaunched = try makeClient(
        recorder: recorder,
        token: token,
        transferDirectory: fixture.transfers,
        connectionPort: port
    )
    await #expect(throws: NodeClientError.self) {
        _ = try await relaunched.resumePendingArchiveRestore(
            jobId: fixture.plan.jobId,
            targetURL: fixture.destination,
            destinationGrantId: fixture.destinationGrantId
        )
    }
    #expect(log.executeRequests == 1)
    #expect(!FileManager.default.fileExists(atPath: fixture.destination.appending(path: "nested/note.txt").path))
    #expect(try await relaunched.pendingArchiveRestores().first?.executionState == .applying)

    let configuration = try NodeConnectionConfiguration(
        baseURL: URL(string: "http://127.0.0.1:\(port)")!,
        apiToken: token
    )
    let model = CovalentAppModel(
        persistence: AppleAppPersistence(
            directoryURL: root.appending(path: "app-state", directoryHint: .isDirectory)
        ),
        client: relaunched,
        configuration: configuration
    )
    await model.start()
    #expect(model.alert?.recovery == .previewRestoreAgain)
    let previewAgain = try #require(model.takeAlertRecovery())
    await previewAgain()
    #expect(log.discardRequests == 1)
    #expect(model.selectedSection == .backups)
    #expect(try await relaunched.pendingArchiveRestores().isEmpty)
    #expect(!FileManager.default.fileExists(atPath: fixture.destination.appending(path: "nested/note.txt").path))

    // A repeated recovery after the confirmed local cleanup remains safe and
    // sends only the idempotent server discard; it never opens a destination.
    try await relaunched.discardUncertainArchiveRestore(jobId: fixture.plan.jobId)
    #expect(log.discardRequests == 2)
}

@Test func appliedRestoreJournalSurvivesResultPersistenceAndAckResponseLossWithoutReextracting() async throws {
    let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString, directoryHint: .isDirectory)
    defer { try? FileManager.default.removeItem(at: root) }
    let fixture = try makeRestoreJournalFixture(root: root, jobId: "restore-result")
    let token = String(repeating: "s", count: 40)
    let log = RestoreJournalRequestLog(losesFirstAcknowledgement: true)
    let recorder = durableRestoreRecorder(plans: [fixture.plan.planId: fixture.plan], archive: fixture.archiveBytes, log: log)
    let port = RecordingURLProtocol.recorder.install(recorder)
    let interrupted = try makeClient(
        recorder: recorder,
        token: token,
        transferDirectory: fixture.transfers,
        restoreDurabilityFailpoint: { point in
            if point == .resultPersistence { throw RestoreJournalInterruption.interrupted }
        },
        connectionPort: port
    )
    await #expect(throws: RestoreJournalInterruption.self) {
        _ = try await interrupted.executeArchiveRestore(
            fixture.plan,
            targetURL: fixture.destination,
            destinationGrantId: fixture.destinationGrantId
        )
    }
    let relaunched = try makeClient(
        recorder: recorder,
        token: token,
        transferDirectory: fixture.transfers,
        connectionPort: port
    )
    #expect(
        try await relaunched.resumePendingArchiveRestore(
            jobId: fixture.plan.jobId,
            targetURL: fixture.destination,
            destinationGrantId: fixture.destinationGrantId
        ) == fixture.result
    )
    #expect(log.executeRequests == 1)
    let acknowledgementInterrupted = try makeClient(
        recorder: recorder,
        token: token,
        transferDirectory: fixture.transfers,
        restoreDurabilityFailpoint: { point in
            if point == .acknowledgement { throw RestoreJournalInterruption.interrupted }
        },
        connectionPort: port
    )
    await #expect(throws: RestoreJournalInterruption.self) {
        try await acknowledgementInterrupted.acknowledgeJob(jobId: fixture.plan.jobId)
    }
    #expect(try await acknowledgementInterrupted.pendingArchiveRestores().count == 1)
    #expect(log.acknowledgementRequests == 0)
    await #expect(throws: NodeClientError.self) {
        try await relaunched.acknowledgeJob(jobId: fixture.plan.jobId)
    }
    #expect(try await relaunched.pendingArchiveRestores().count == 1)
    try await relaunched.acknowledgeJob(jobId: fixture.plan.jobId)
    #expect(try await relaunched.pendingArchiveRestores().isEmpty)
    #expect(log.acknowledgementRequests == 2)
}

@Test func restoreAcknowledgementRejectsADanglingJournalSymlinkBeforeNetworkUse() async throws {
    let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString, directoryHint: .isDirectory)
    defer { try? FileManager.default.removeItem(at: root) }
    let fixture = try makeRestoreJournalFixture(root: root, jobId: "restore-ack-symlink")
    let token = String(repeating: "y", count: 40)
    let log = RestoreJournalRequestLog()
    let recorder = durableRestoreRecorder(plans: [fixture.plan.planId: fixture.plan], archive: fixture.archiveBytes, log: log)
    let port = RecordingURLProtocol.recorder.install(recorder)
    let interrupted = try makeClient(
        recorder: recorder,
        token: token,
        transferDirectory: fixture.transfers,
        restoreDurabilityFailpoint: { point in
            if point == .resultPersistence { throw RestoreJournalInterruption.interrupted }
        },
        connectionPort: port
    )
    await #expect(throws: RestoreJournalInterruption.self) {
        _ = try await interrupted.executeArchiveRestore(
            fixture.plan,
            targetURL: fixture.destination,
            destinationGrantId: fixture.destinationGrantId
        )
    }
    let journal = try #require(
        FileManager.default.contentsOfDirectory(at: fixture.transfers, includingPropertiesForKeys: nil)
            .first { $0.pathExtension == "json" }
    )
    try FileManager.default.removeItem(at: journal)
    try FileManager.default.createSymbolicLink(
        at: journal,
        withDestinationURL: fixture.transfers.appending(path: "missing-journal")
    )
    let relaunched = try makeClient(
        recorder: recorder,
        token: token,
        transferDirectory: fixture.transfers,
        connectionPort: port
    )
    await #expect(throws: NodeClientError.self) {
        try await relaunched.acknowledgeJob(jobId: fixture.plan.jobId)
    }
    #expect(log.acknowledgementRequests == 0)
}

@Test func restoreJournalRejectsPlanTokenAndDestinationSubstitutionBeforeNetworkUse() async throws {
    let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString, directoryHint: .isDirectory)
    defer { try? FileManager.default.removeItem(at: root) }
    let fixture = try makeRestoreJournalFixture(root: root, jobId: "restore-substitution")
    let token = String(repeating: "t", count: 40)
    let log = RestoreJournalRequestLog()
    let recorder = durableRestoreRecorder(plans: [fixture.plan.planId: fixture.plan], archive: fixture.archiveBytes, log: log)
    let port = RecordingURLProtocol.recorder.install(recorder)
    let interrupted = try makeClient(
        recorder: recorder,
        token: token,
        transferDirectory: fixture.transfers,
        restoreDurabilityFailpoint: { point in
            if point == .request { throw RestoreJournalInterruption.interrupted }
        },
        connectionPort: port
    )
    await #expect(throws: RestoreJournalInterruption.self) {
        _ = try await interrupted.executeArchiveRestore(
            fixture.plan,
            targetURL: fixture.destination,
            destinationGrantId: fixture.destinationGrantId
        )
    }
    let changedPlan = RestorePlan(reference: fixture.plan.reference, entries: [])
    let sameConnection = try makeClient(
        recorder: recorder,
        token: token,
        transferDirectory: fixture.transfers,
        connectionPort: port
    )
    await #expect(throws: NodeClientError.self) {
        _ = try await sameConnection.executeArchiveRestore(
            changedPlan,
            targetURL: fixture.destination,
            destinationGrantId: fixture.destinationGrantId
        )
    }
    let substitute = root.appending(path: "substitute", directoryHint: .isDirectory)
    try FileManager.default.createDirectory(at: substitute, withIntermediateDirectories: true)
    await #expect(throws: NodeClientError.self) {
        _ = try await sameConnection.resumePendingArchiveRestore(
            jobId: fixture.plan.jobId,
            targetURL: substitute,
            destinationGrantId: fixture.destinationGrantId
        )
    }
    let wrongToken = try makeClient(
        recorder: recorder,
        token: String(repeating: "u", count: 40),
        transferDirectory: fixture.transfers,
        connectionPort: port
    )
    #expect(try await wrongToken.pendingArchiveRestores().isEmpty)
    let quarantine = fixture.transfers.appending(path: "restore-quarantine", directoryHint: .isDirectory)
    #expect(
        try FileManager.default.contentsOfDirectory(at: quarantine, includingPropertiesForKeys: nil)
            .contains { $0.pathExtension == "json" }
    )
    #expect(log.executeRequests == 0)
    #expect(log.acknowledgementRequests == 0)
}

@Test func restoreJournalMACRejectsMutationOfEveryReplayFieldBeforeNetworkOrDestinationUse() async throws {
    let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString, directoryHint: .isDirectory)
    defer { try? FileManager.default.removeItem(at: root) }
    let fixture = try makeRestoreJournalFixture(root: root, jobId: "restore-mac-fields")
    let token = String(repeating: "m", count: 40)
    let log = RestoreJournalRequestLog()
    let recorder = durableRestoreRecorder(plans: [fixture.plan.planId: fixture.plan], archive: fixture.archiveBytes, log: log)
    let port = RecordingURLProtocol.recorder.install(recorder)
    let staging = try makeClient(
        recorder: recorder,
        token: token,
        transferDirectory: fixture.transfers,
        restoreDurabilityFailpoint: { point in
            if point == .download { throw RestoreJournalInterruption.interrupted }
        },
        connectionPort: port
    )
    await #expect(throws: RestoreJournalInterruption.self) {
        _ = try await staging.executeArchiveRestore(
            fixture.plan,
            targetURL: fixture.destination,
            destinationGrantId: fixture.destinationGrantId
        )
    }
    let entries = try FileManager.default.contentsOfDirectory(
        at: fixture.transfers,
        includingPropertiesForKeys: nil
    )
    let journalURL = try #require(entries.first { $0.pathExtension == "json" })
    let archiveURL = try #require(entries.first { $0.pathExtension == "zip" })
    let journalBytes = try Data(contentsOf: journalURL)
    let archiveBytes = try Data(contentsOf: archiveURL)

    typealias JournalMutation = (inout [String: Any]) -> Void
    let mutations: [(String, JournalMutation)] = [
        ("schemaVersion", { $0["schemaVersion"] = 3 }),
        ("plan", { journal in
            var plan = journal["plan"] as! [String: Any]
            var reference = plan["reference"] as! [String: Any]
            reference["signature"] = "substituted-signature"
            plan["reference"] = reference
            journal["plan"] = plan
        }),
        ("destinationGrantId", { $0["destinationGrantId"] = UUID().uuidString.lowercased() }),
        ("destinationInventory", { journal in
            var inventory = journal["destinationInventory"] as! [String: Any]
            inventory["rootIdentity"] = "substituted-root"
            journal["destinationInventory"] = inventory
        }),
        ("connectionBinding", { $0["connectionBinding"] = String(repeating: "0", count: 64) }),
        ("createdAt", { $0["createdAt"] = "2030-01-01T00:00:00Z" }),
        // This transition is structurally valid without a MAC and would make
        // startup trust the accepted response and ACK without extraction.
        ("executionState", { $0["executionState"] = "applied" }),
        ("archivePath", { $0["archivePath"] = "/private/tmp/substituted.zip" }),
        ("archiveLength", { journal in
            let length = journal["archiveLength"] as! NSNumber
            journal["archiveLength"] = length.uint64Value + 1
        }),
        ("archiveDigest", { $0["archiveDigest"] = String(repeating: "f", count: 64) }),
        ("acceptedResponse", { journal in
            var response = journal["acceptedResponse"] as! [String: Any]
            response["bytesWritten"] = 9_999
            journal["acceptedResponse"] = response
        }),
    ]

    for (field, mutate) in mutations {
        var journal = try #require(JSONSerialization.jsonObject(with: journalBytes) as? [String: Any])
        mutate(&journal)
        let changed = try JSONSerialization.data(withJSONObject: journal, options: [.sortedKeys])
        try writeOwnerOnlyTestFile(changed, to: journalURL)
        try writeOwnerOnlyTestFile(archiveBytes, to: archiveURL)

        let relaunched = try makeClient(
            recorder: recorder,
            token: token,
            transferDirectory: fixture.transfers,
            connectionPort: port
        )
        #expect(try await relaunched.pendingArchiveRestores().isEmpty, "Field was accepted: \(field)")
        let quarantine = fixture.transfers.appending(path: "restore-quarantine", directoryHint: .isDirectory)
        let quarantined = try FileManager.default.contentsOfDirectory(
            at: quarantine,
            includingPropertiesForKeys: nil
        )
        #expect(quarantined.contains { $0.pathExtension == "json" }, "Journal was not quarantined: \(field)")
        #expect(quarantined.contains { $0.pathExtension == "zip" }, "Archive was not quarantined: \(field)")
        try FileManager.default.removeItem(at: quarantine)
    }
    #expect(log.executeRequests == 1)
    #expect(log.acknowledgementRequests == 0)
    #expect(log.discardRequests == 0)
    #expect(!FileManager.default.fileExists(atPath: fixture.destination.appending(path: "nested/note.txt").path))
}

@Test @MainActor func appStartupReconcilesTwelveAppliedRestoresWhileQuarantiningOneInvalidJournal() async throws {
    let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString, directoryHint: .isDirectory)
    defer { try? FileManager.default.removeItem(at: root) }
    let transfers = root.appending(path: "transfers", directoryHint: .isDirectory)
    let source = root.appending(path: "source", directoryHint: .isDirectory)
    try FileManager.default.createDirectory(at: source.appending(path: "nested"), withIntermediateDirectories: true)
    try Data("durable restore\n".utf8).write(to: source.appending(path: "nested/note.txt"))
    let archive = try AppleArchiveTransfer.makeBackupArchive(sourceURL: source)
    defer { try? FileManager.default.removeItem(at: archive) }
    let archiveBytes = try Data(contentsOf: archive)
    var fixtures: [RestoreJournalFixture] = []
    for index in 0..<12 {
        let destination = root.appending(path: "destination-\(index)", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: destination, withIntermediateDirectories: true)
        fixtures.append(try makeRestoreJournalFixture(
            root: root,
            jobId: "restore-pressure-\(index)",
            destination: destination,
            transfers: transfers,
            archiveBytes: archiveBytes
        ))
    }
    let invalidFixture = try makeRestoreJournalFixture(
        root: root,
        jobId: "restore-pressure-invalid",
        destination: root.appending(path: "destination-invalid", directoryHint: .isDirectory),
        transfers: transfers,
        archiveBytes: archiveBytes
    )
    let token = String(repeating: "q", count: 40)
    let log = RestoreJournalRequestLog()
    let recorder = durableRestoreRecorder(
        plans: Dictionary(uniqueKeysWithValues: (fixtures + [invalidFixture]).map { ($0.plan.planId, $0.plan) }),
        archive: archiveBytes,
        log: log
    )
    let port = RecordingURLProtocol.recorder.install(recorder)
    for fixture in fixtures {
        let interrupted = try makeClient(
            recorder: recorder,
            token: token,
            transferDirectory: transfers,
            restoreDurabilityFailpoint: { point in
                if point == .resultPersistence { throw RestoreJournalInterruption.interrupted }
            },
            connectionPort: port
        )
        await #expect(throws: RestoreJournalInterruption.self) {
            _ = try await interrupted.executeArchiveRestore(
                fixture.plan,
                targetURL: fixture.destination,
                destinationGrantId: fixture.destinationGrantId
            )
        }
        try Data("post-apply sentinel \(fixture.plan.jobId)".utf8).write(
            to: fixture.destination.appending(path: "nested/note.txt")
        )
    }

    let existingRecords = Set(
        try FileManager.default.contentsOfDirectory(at: transfers, includingPropertiesForKeys: nil)
            .filter { $0.pathExtension == "json" }
    )
    let invalidStaging = try makeClient(
        recorder: recorder,
        token: token,
        transferDirectory: transfers,
        restoreDurabilityFailpoint: { point in
            if point == .request { throw RestoreJournalInterruption.interrupted }
        },
        connectionPort: port
    )
    await #expect(throws: RestoreJournalInterruption.self) {
        _ = try await invalidStaging.executeArchiveRestore(
            invalidFixture.plan,
            targetURL: invalidFixture.destination,
            destinationGrantId: invalidFixture.destinationGrantId
        )
    }
    let invalidRecord = try #require(
        FileManager.default.contentsOfDirectory(at: transfers, includingPropertiesForKeys: nil)
            .first { $0.pathExtension == "json" && !existingRecords.contains($0) }
    )
    var invalidJSON = try #require(
        JSONSerialization.jsonObject(with: Data(contentsOf: invalidRecord)) as? [String: Any]
    )
    invalidJSON["authenticationCode"] = "invalid"
    try writeOwnerOnlyTestFile(
        JSONSerialization.data(withJSONObject: invalidJSON, options: [.sortedKeys]),
        to: invalidRecord
    )

    let finalClient = try makeClient(
        recorder: recorder,
        token: token,
        transferDirectory: transfers,
        connectionPort: port
    )
    let configuration = try NodeConnectionConfiguration(
        baseURL: URL(string: "http://127.0.0.1:\(port)")!,
        apiToken: token
    )
    let model = CovalentAppModel(
        persistence: AppleAppPersistence(
            directoryURL: root.appending(path: "app-state", directoryHint: .isDirectory)
        ),
        client: finalClient,
        configuration: configuration
    )
    await model.start()
    #expect(model.phase == .ready)
    #expect(try await finalClient.pendingArchiveRestores().isEmpty)
    #expect(log.executeRequests == 12)
    #expect(log.acknowledgementRequests == 12)
    #expect(log.discardRequests == 0)
    for fixture in fixtures {
        #expect(
            try String(
                contentsOf: fixture.destination.appending(path: "nested/note.txt"),
                encoding: .utf8
            ) == "post-apply sentinel \(fixture.plan.jobId)"
        )
    }
    let quarantine = transfers.appending(path: "restore-quarantine", directoryHint: .isDirectory)
    let quarantined = try FileManager.default.contentsOfDirectory(at: quarantine, includingPropertiesForKeys: nil)
    #expect(quarantined.filter { $0.pathExtension == "json" }.count == 1)
}

@Test func backupResponsesRequireAcknowledgementHeaderAndExactMetadata() async throws {
    let token = String(repeating: "h", count: 40)
    let request = BackupRequest(
        sourceRoot: "/private/source",
        displayName: "Header",
        snapshotId: "header-1",
        jobId: "apple-header-required",
        selectedProviderIds: []
    )
    let missingHeader = RequestRecorder { urlRequest in
        TestResponse.response(
            urlRequest,
            status: 200,
            json: backupResponseJSON(backupId: UUID(), snapshotId: request.snapshotId)
        )
    }
    let first = try makeClient(recorder: missingHeader, token: token)
    await #expect(throws: NodeClientError.invalidResponse) {
        _ = try await first.createBackup(request)
    }

    let mismatched = RequestRecorder { urlRequest in
        TestResponse.response(
            urlRequest,
            status: 200,
            json: backupResponseJSON(backupId: UUID(), snapshotId: "wrong-snapshot"),
            headers: [AppleArchiveTransfer.jobAcknowledgementRequiredHeader: "true"]
        )
    }
    let second = try makeClient(recorder: mismatched, token: token)
    await #expect(throws: NodeClientError.invalidResponse) {
        _ = try await second.createBackup(request)
    }
}

@Test @MainActor func appRelaunchPersistsAcceptedSnapshotBeforeAcknowledgingServerReceipt() async throws {
    let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString, directoryHint: .isDirectory)
    let source = root.appending(path: "source", directoryHint: .isDirectory)
    let transfers = root.appending(path: "transfers", directoryHint: .isDirectory)
    let appState = root.appending(path: "app-state", directoryHint: .isDirectory)
    defer { try? FileManager.default.removeItem(at: root) }
    try FileManager.default.createDirectory(at: source, withIntermediateDirectories: true)
    try Data("app relaunch".utf8).write(to: source.appending(path: "note.txt"))
    let grant = try SelectedDirectoryGrant.capture(url: source, purpose: .backupSource)
    let persistence = AppleAppPersistence(directoryURL: appState)
    try await persistence.saveDirectoryGrants([grant])
    let metadata = ArchiveBackupMetadata(
        displayName: "Relaunch",
        snapshotId: "app-relaunch-1",
        jobId: "apple-app-relaunch",
        selectedProviderIds: []
    )
    let token = String(repeating: "u", count: 40)
    let stageRecorder = RequestRecorder { request in
        TestResponse.response(
            request,
            status: 200,
            json: backupResponseJSON(backupId: UUID(), snapshotId: metadata.snapshotId),
            headers: [AppleArchiveTransfer.jobAcknowledgementRequiredHeader: "true"]
        )
    }
    let stagingClient = try makeClient(
        recorder: stageRecorder,
        token: token,
        transferDirectory: transfers
    )
    _ = try await stagingClient.createBackupArchive(
        sourceURL: source,
        metadata: metadata,
        sourceGrantId: grant.id
    )

    let snapshotHistory = appState.appending(path: "snapshot-history.json")
    let relaunchRecorder = RequestRecorder(removeAfterRequest: false) { request in
        switch request.url?.path {
        case "/api/v1/status":
            return TestResponse.response(
                request,
                status: 200,
                json: #"{"deviceName":"Apple test","protocolVersion":1,"lanDiscovery":false,"platformTier":"tier1","state":"ready"}"#
            )
        case "/api/v1/config/export":
            return TestResponse.response(
                request,
                status: 200,
                json: #"{"schemaVersion":1,"deviceName":"Apple test","lanDiscoveryEnabled":false,"rememberedBackups":[]}"#
            )
        case "/api/v1/providers", "/api/v1/backups":
            return TestResponse.response(request, status: 200, json: "[]")
        case "/api/v1/discovery":
            return TestResponse.response(request, status: 200, json: "[]")
        case "/api/v1/jobs/acknowledge":
            #expect(FileManager.default.fileExists(atPath: snapshotHistory.path))
            let saved = try Data(contentsOf: snapshotHistory)
            #expect(String(decoding: saved, as: UTF8.self).contains(metadata.snapshotId))
            return TestResponse.response(request, status: 204, json: "")
        default:
            Issue.record("Unexpected app-relaunch request: \(request.url?.path ?? "missing")")
            return TestResponse.response(request, status: 500, json: "{}")
        }
    }
    let configuration = try NodeConnectionConfiguration(
        baseURL: URL(string: "http://127.0.0.1:23991")!,
        apiToken: token
    )
    let sessionConfiguration = URLSessionConfiguration.ephemeral
    sessionConfiguration.protocolClasses = [RecordingURLProtocol.self]
    let port = RecordingURLProtocol.recorder.install(relaunchRecorder)
    let relaunchedConfiguration = try NodeConnectionConfiguration(
        baseURL: URL(string: "http://127.0.0.1:\(port)")!,
        apiToken: configuration.apiToken
    )
    let relaunchedClient = NodeClient(
        configuration: relaunchedConfiguration,
        session: URLSession(configuration: sessionConfiguration),
        transferDirectory: transfers
    )
    let model = CovalentAppModel(
        persistence: persistence,
        client: relaunchedClient,
        configuration: relaunchedConfiguration
    )
    await model.start()
    #expect(model.snapshots.count == 1)
    #expect(model.snapshots[0].snapshotId == metadata.snapshotId)
    #expect(try await relaunchedClient.pendingArchiveBackups().isEmpty)
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

@Test func archiveRestoreAppliesSignedSkipReplaceAndRenamePolicies() throws {
    let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString, directoryHint: .isDirectory)
    defer { try? FileManager.default.removeItem(at: root) }
    let target = root.appending(path: "target", directoryHint: .isDirectory)
    let source = root.appending(path: "source", directoryHint: .isDirectory)
    try FileManager.default.createDirectory(at: target, withIntermediateDirectories: true)
    try FileManager.default.createDirectory(at: source, withIntermediateDirectories: true)
    let existing = target.appending(path: "note.txt")
    try Data("old".utf8).write(to: existing)

    let skipInventory = try AppleArchiveTransfer.makeTargetInventory(targetURL: target)
    let emptyArchive = try AppleArchiveTransfer.makeBackupArchive(sourceURL: source)
    defer { try? FileManager.default.removeItem(at: emptyArchive) }
    try AppleArchiveTransfer.extractRestoreArchive(
        emptyArchive,
        to: target,
        plan: policyRestorePlan(
            inventory: skipInventory,
            policy: .skip,
            entries: [RestorePreviewEntry(
                sourcePath: "note.txt",
                destinationPath: "note.txt",
                kind: .file,
                action: .skipFile
            )]
        ),
        expectedInventory: skipInventory
    )
    #expect(try String(contentsOf: existing, encoding: .utf8) == "old")

    try Data("replacement".utf8).write(to: source.appending(path: "note.txt"))
    let replaceArchive = try AppleArchiveTransfer.makeBackupArchive(sourceURL: source)
    defer { try? FileManager.default.removeItem(at: replaceArchive) }
    let replaceInventory = try AppleArchiveTransfer.makeTargetInventory(targetURL: target)
    try AppleArchiveTransfer.extractRestoreArchive(
        replaceArchive,
        to: target,
        plan: policyRestorePlan(
            inventory: replaceInventory,
            policy: .replace,
            entries: [RestorePreviewEntry(
                sourcePath: "note.txt",
                destinationPath: "note.txt",
                kind: .file,
                action: .replaceFile
            )]
        ),
        expectedInventory: replaceInventory
    )
    #expect(try String(contentsOf: existing, encoding: .utf8) == "replacement")

    try FileManager.default.removeItem(at: source.appending(path: "note.txt"))
    try Data("renamed".utf8).write(to: source.appending(path: "note.txt.covalent-restored-1"))
    let renameArchive = try AppleArchiveTransfer.makeBackupArchive(sourceURL: source)
    defer { try? FileManager.default.removeItem(at: renameArchive) }
    let renameInventory = try AppleArchiveTransfer.makeTargetInventory(targetURL: target)
    try AppleArchiveTransfer.extractRestoreArchive(
        renameArchive,
        to: target,
        plan: policyRestorePlan(
            inventory: renameInventory,
            policy: .rename,
            entries: [RestorePreviewEntry(
                sourcePath: "note.txt",
                destinationPath: "note.txt.covalent-restored-1",
                kind: .file,
                action: .renameFile
            )]
        ),
        expectedInventory: renameInventory
    )
    #expect(try String(contentsOf: existing, encoding: .utf8) == "replacement")
    #expect(try String(contentsOf: target.appending(path: "note.txt.covalent-restored-1"), encoding: .utf8) == "renamed")
}

@Test func archiveRestoreRollsBackAtomicReplacementWhenLaterEntryFails() throws {
    let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString, directoryHint: .isDirectory)
    defer { try? FileManager.default.removeItem(at: root) }
    let target = root.appending(path: "target", directoryHint: .isDirectory)
    let source = root.appending(path: "source", directoryHint: .isDirectory)
    try FileManager.default.createDirectory(at: target, withIntermediateDirectories: true)
    try FileManager.default.createDirectory(at: source, withIntermediateDirectories: true)
    try Data("old".utf8).write(to: target.appending(path: "a-existing.txt"))
    try Data("new".utf8).write(to: source.appending(path: "a-existing.txt"))
    try Data("later".utf8).write(to: source.appending(path: "z-after.txt"))
    let archive = try AppleArchiveTransfer.makeBackupArchive(sourceURL: source)
    defer { try? FileManager.default.removeItem(at: archive) }
    let inventory = try AppleArchiveTransfer.makeTargetInventory(targetURL: target)
    let plan = policyRestorePlan(
        inventory: inventory,
        policy: .replace,
        entries: [
            RestorePreviewEntry(
                sourcePath: "a-existing.txt",
                destinationPath: "a-existing.txt",
                kind: .file,
                action: .replaceFile
            ),
            RestorePreviewEntry(
                sourcePath: "z-after.txt",
                destinationPath: "z-after.txt",
                kind: .file,
                action: .createFile
            ),
        ]
    )
    #expect(throws: AppleArchiveTransferError.destinationChanged) {
        try AppleArchiveTransfer.extractRestoreArchive(
            archive,
            to: target,
            plan: plan,
            expectedInventory: inventory,
            beforeEntry: { components in
                if components == ["z-after.txt"] {
                    throw AppleArchiveTransferError.destinationChanged
                }
            }
        )
    }
    #expect(try String(contentsOf: target.appending(path: "a-existing.txt"), encoding: .utf8) == "old")
    #expect(!FileManager.default.fileExists(atPath: target.appending(path: "z-after.txt").path))
}

private func policyRestorePlan(
    inventory: AppleArchiveTransfer.TargetInventoryDraft,
    policy: ConflictPolicy,
    entries: [RestorePreviewEntry]
) -> RestorePlan {
    RestorePlan(
        reference: RestorePlanReference(
            planId: String(repeating: "c", count: 64),
            backupId: UUID(),
            snapshotId: "snapshot-policy",
            authorizedRoot: "/",
            manifestDigest: String(repeating: "a", count: 64),
            conflictPolicy: policy,
            jobId: "restore-policy",
            planDigest: String(repeating: "b", count: 64),
            signerDeviceId: UUID(),
            signature: "test",
            totalEntries: entries.count,
            targetInventory: TargetInventoryBinding(
                schemaVersion: 1,
                rootIdentity: inventory.rootIdentity,
                entryCount: UInt64(inventory.entries.count),
                totalBytes: inventory.totalBytes,
                inventoryDigest: String(repeating: "d", count: 64),
                actionsDigest: String(repeating: "e", count: 64)
            )
        ),
        entries: entries
    )
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
            totalEntries: 2,
            targetInventory: nil
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

private enum RestoreJournalInterruption: Error {
    case interrupted
}

private struct RestoreJournalFixture {
    let plan: RestorePlan
    let destination: URL
    let destinationGrantId: UUID
    let transfers: URL
    let archiveBytes: Data
    let result: RestoreResponse
}

private func makeRestoreJournalFixture(
    root: URL,
    jobId: String,
    destination providedDestination: URL? = nil,
    transfers providedTransfers: URL? = nil,
    archiveBytes providedArchiveBytes: Data? = nil
) throws -> RestoreJournalFixture {
    let destination = providedDestination ?? root.appending(path: "destination", directoryHint: .isDirectory)
    let transfers = providedTransfers ?? root.appending(path: "transfers", directoryHint: .isDirectory)
    try FileManager.default.createDirectory(at: destination, withIntermediateDirectories: true)
    let draft = try AppleArchiveTransfer.makeTargetInventory(targetURL: destination)
    let entries = [
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
    let plan = RestorePlan(
        reference: RestorePlanReference(
            planId: String(repeating: "c", count: 32) + UUID().uuidString.lowercased().replacingOccurrences(of: "-", with: ""),
            backupId: UUID(),
            snapshotId: "journal-\(jobId)",
            authorizedRoot: "/",
            manifestDigest: String(repeating: "a", count: 64),
            conflictPolicy: .fail,
            jobId: jobId,
            planDigest: String(repeating: "b", count: 64),
            signerDeviceId: UUID(),
            signature: "journal-test",
            totalEntries: entries.count,
            targetInventory: TargetInventoryBinding(
                schemaVersion: 1,
                rootIdentity: draft.rootIdentity,
                entryCount: UInt64(draft.entries.count),
                totalBytes: draft.totalBytes,
                inventoryDigest: String(repeating: "d", count: 64),
                actionsDigest: String(repeating: "e", count: 64)
            )
        ),
        entries: entries
    )
    let archiveBytes: Data
    if let providedArchiveBytes {
        archiveBytes = providedArchiveBytes
    } else {
        let source = root.appending(path: "source", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: source.appending(path: "nested"), withIntermediateDirectories: true)
        try Data("durable restore\n".utf8).write(to: source.appending(path: "nested/note.txt"))
        let archive = try AppleArchiveTransfer.makeBackupArchive(sourceURL: source)
        defer { try? FileManager.default.removeItem(at: archive) }
        archiveBytes = try Data(contentsOf: archive)
    }
    return RestoreJournalFixture(
        plan: plan,
        destination: destination,
        destinationGrantId: UUID(),
        transfers: transfers,
        archiveBytes: archiveBytes,
        result: RestoreResponse(
            filesRestored: 1,
            directoriesCreated: 1,
            filesSkipped: 0,
            bytesWritten: UInt64("durable restore\n".utf8.count),
            rejectedProviderCopies: 0
        )
    )
}

private final class RestoreJournalRequestLog: @unchecked Sendable {
    private let lock = NSLock()
    private var executeCount = 0
    private var acknowledgementCount = 0
    private var discardCount = 0
    private var shouldLoseAcknowledgement: Bool

    init(losesFirstAcknowledgement: Bool = false) {
        shouldLoseAcknowledgement = losesFirstAcknowledgement
    }

    var executeRequests: Int { lock.withLock { executeCount } }
    var acknowledgementRequests: Int { lock.withLock { acknowledgementCount } }
    var discardRequests: Int { lock.withLock { discardCount } }

    func recordExecute() { lock.withLock { executeCount += 1 } }
    func recordDiscard() { lock.withLock { discardCount += 1 } }

    func acknowledgeShouldLoseResponse() -> Bool {
        lock.withLock {
            acknowledgementCount += 1
            defer { shouldLoseAcknowledgement = false }
            return shouldLoseAcknowledgement
        }
    }
}

private func durableRestoreRecorder(
    plans: [String: RestorePlan],
    archive: Data,
    log: RestoreJournalRequestLog
) -> RequestRecorder {
    RequestRecorder(removeAfterRequest: false) { request in
        let path = request.url?.path ?? ""
        switch path {
        case "/api/v1/status":
            return TestResponse.response(
                request,
                status: 200,
                json: #"{"deviceName":"Apple test","protocolVersion":1,"lanDiscovery":false,"platformTier":"tier1","state":"ready"}"#
            )
        case "/api/v1/config/export":
            return TestResponse.response(
                request,
                status: 200,
                json: #"{"schemaVersion":1,"deviceName":"Apple test","lanDiscoveryEnabled":false,"rememberedBackups":[]}"#
            )
        case "/api/v1/providers", "/api/v1/backups", "/api/v1/discovery":
            return TestResponse.response(request, status: 200, json: "[]")
        case "/api/v1/restores/archive/inventories":
            let body = try #require(requestBody(request))
            let decoded = try #require(JSONSerialization.jsonObject(with: body) as? [String: Any])
            let jobId = try #require(decoded["jobId"] as? String)
            return TestResponse.response(
                request,
                status: 200,
                json: #"{"inventoryId":"inventory-\#(jobId)","jobId":"\#(jobId)","nextOffset":0}"#
            )
        case let value where value.hasSuffix("/finalize") && value.contains("/api/v1/restores/archive/inventories/"):
            let body = try #require(requestBody(request))
            let decoded = try #require(JSONSerialization.jsonObject(with: body) as? [String: Any])
            let jobId = try #require(decoded["jobId"] as? String)
            let plan = try #require(plans.values.first { $0.jobId == jobId })
            let binding = try #require(plan.targetInventory)
            let data = try JSONSerialization.data(withJSONObject: [
                "inventoryId": "inventory-\(jobId)",
                "jobId": jobId,
                "schemaVersion": binding.schemaVersion,
                "rootIdentity": binding.rootIdentity,
                "entryCount": binding.entryCount,
                "totalBytes": binding.totalBytes,
                "inventoryDigest": binding.inventoryDigest,
            ])
            return TestResponse.data(request, status: 200, body: data, headers: ["Content-Type": "application/json"])
        case "/api/v1/restores/archive/preview":
            let body = try #require(requestBody(request))
            let decoded = try #require(JSONSerialization.jsonObject(with: body) as? [String: Any])
            let jobId = try #require(decoded["jobId"] as? String)
            let plan = try #require(plans.values.first { $0.jobId == jobId })
            return TestResponse.data(
                request,
                status: 200,
                body: try JSONEncoder().encode(plan.reference),
                headers: [
                    "Content-Type": "application/json",
                    "Cache-Control": "no-store",
                    AppleArchiveTransfer.restorePlanIdHeader: plan.planId,
                    AppleArchiveTransfer.restorePlanDigestHeader: plan.planDigest,
                ]
            )
        case "/api/v1/restores/archive/execute":
            let body = try #require(requestBody(request))
            let decoded = try #require(JSONSerialization.jsonObject(with: body) as? [String: String])
            let planId = try #require(decoded["planId"])
            let plan = try #require(plans[planId])
            log.recordExecute()
            let result = RestoreResponse(
                filesRestored: 1,
                directoriesCreated: 1,
                filesSkipped: 0,
                bytesWritten: UInt64("durable restore\n".utf8.count),
                rejectedProviderCopies: 0
            )
            return TestResponse.data(
                request,
                status: 200,
                body: archive,
                headers: [
                    "Content-Type": AppleArchiveTransfer.restoreContentType,
                    AppleArchiveTransfer.restoreResultHeader: restoreResultHeader(result),
                    AppleArchiveTransfer.restorePlanIdHeader: plan.planId,
                    AppleArchiveTransfer.restorePlanDigestHeader: plan.planDigest,
                    AppleArchiveTransfer.jobAcknowledgementRequiredHeader: "true",
                ]
            )
        case "/api/v1/jobs/acknowledge":
            if log.acknowledgeShouldLoseResponse() {
                // The server has accepted the request, but the client cannot
                // distinguish the lost response from a failed request.
                throw URLError(.networkConnectionLost)
            }
            return TestResponse.response(request, status: 204, json: "")
        case "/api/v1/jobs/discard":
            log.recordDiscard()
            return TestResponse.response(request, status: 204, json: "")
        default:
            Issue.record("Unexpected durable restore request: \(path)")
            return TestResponse.response(request, status: 500, json: "{}")
        }
    }
}

private func restoreResultHeader(_ result: RestoreResponse) -> String {
    try! JSONEncoder().encode(result).base64EncodedString()
        .replacingOccurrences(of: "+", with: "-")
        .replacingOccurrences(of: "/", with: "_")
        .trimmingCharacters(in: CharacterSet(charactersIn: "="))
}

private func makeClient(
    recorder: RequestRecorder,
    token: String?,
    transferDirectory: URL? = nil,
    restoreDurabilityFailpoint: (@Sendable (RestoreDurabilityCheckpoint) throws -> Void)? = nil,
    connectionPort: Int? = nil
) throws -> NodeClient {
    let sessionConfiguration = URLSessionConfiguration.ephemeral
    sessionConfiguration.protocolClasses = [RecordingURLProtocol.self]
    let port = connectionPort ?? RecordingURLProtocol.recorder.install(recorder)
    let session = URLSession(configuration: sessionConfiguration)
    let configuration = try NodeConnectionConfiguration(
        baseURL: URL(string: "http://127.0.0.1:\(port)")!,
        apiToken: token
    )
    return NodeClient(
        configuration: configuration,
        session: session,
        transferDirectory: transferDirectory,
        restoreDurabilityFailpoint: restoreDurabilityFailpoint
    )
}

private func writeOwnerOnlyTestFile(_ data: Data, to url: URL) throws {
    try data.write(to: url, options: .atomic)
    try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: url.path)
}

private func backupResponseJSON(backupId: UUID, snapshotId: String) -> String {
    #"{"backupId":"\#(backupId.uuidString.lowercased())","snapshotId":"\#(snapshotId)","entries":1,"bytesRead":64,"chunksStored":1,"chunksDeduplicated":0,"selectedProviders":0,"degradedFailures":0}"#
}

private func decodeBase64URL(_ value: String) -> Data? {
    var standard = value.replacingOccurrences(of: "-", with: "+")
        .replacingOccurrences(of: "_", with: "/")
    standard += String(repeating: "=", count: (4 - standard.count % 4) % 4)
    return Data(base64Encoded: standard)
}

/// Test harness tokens are intentionally passed by owner-only file path, not
/// through a process argument or environment value. Keep this check aligned
/// with the node's `--api-token-file` contract.
private func readOwnerOnlyTestToken(at path: String) throws -> String {
    let fileManager = FileManager.default
    let attributes = try fileManager.attributesOfItem(atPath: path)
    guard attributes[.type] as? FileAttributeType == .typeRegular,
          let owner = attributes[.ownerAccountID] as? NSNumber,
          owner.uint32Value == getuid(),
          let permissions = attributes[.posixPermissions] as? NSNumber,
          permissions.uint16Value & 0o077 == 0
    else {
        throw NodeClientError.invalidToken
    }
    return try SecureNodeConnectionStore.parseTokenFile(
        Data(contentsOf: URL(fileURLWithPath: path), options: .uncached)
    )
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

    static func data(
        _ request: URLRequest,
        status: Int,
        body: Data,
        headers: [String: String] = [:]
    ) -> (HTTPURLResponse, Data) {
        let response = HTTPURLResponse(
            url: request.url!,
            statusCode: status,
            httpVersion: "HTTP/1.1",
            headerFields: ["Content-Type": "application/octet-stream"].merging(headers) { _, new in new }
        )!
        return (response, body)
    }
}

/// Collects restore progress callbacks, which arrive off the main actor.
private final class RestoreProgressLog: @unchecked Sendable {
    private let lock = NSLock()
    private var recorded: [TransferProgressSnapshot] = []

    func record(_ snapshot: TransferProgressSnapshot) {
        lock.lock()
        recorded.append(snapshot)
        lock.unlock()
    }

    var snapshots: [TransferProgressSnapshot] {
        lock.lock()
        defer { lock.unlock() }
        return recorded
    }
}
