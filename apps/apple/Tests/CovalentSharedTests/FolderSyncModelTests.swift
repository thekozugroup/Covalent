import Foundation
import Testing
@testable import CovalentShared

@MainActor
private final class FolderGrantBootstrapper: LocalNodeBootstrapping {
    let configuration: NodeConnectionConfiguration
    var restartFailuresRemaining = 0
    private(set) var restartCalls = 0
    private(set) var pendingRepairRestartCalls = 0
    private(set) var lastPendingRepairGrants: [SelectedDirectoryGrant] = []

    init(configuration: NodeConnectionConfiguration) {
        self.configuration = configuration
    }

    func startupDisposition() throws -> LocalNodeStartupDisposition { .existingIdentity }

    func prepareFolderSyncDirectoryGrants(_ grants: [SelectedDirectoryGrant]) async throws {}

    func start(mode: LocalNodeStartupMode) async throws -> NodeConnectionConfiguration { configuration }

    func restartForFolderSyncDirectoryGrants(_ grants: [SelectedDirectoryGrant]) async throws -> NodeConnectionConfiguration {
        restartCalls += 1
        if restartFailuresRemaining > 0 {
            restartFailuresRemaining -= 1
            throw FolderGrantTestError.restartFailed
        }
        return configuration
    }

    func restartForPendingFolderRepairDirectoryGrants(
        _ grants: [SelectedDirectoryGrant]
    ) async throws -> NodeConnectionConfiguration {
        pendingRepairRestartCalls += 1
        lastPendingRepairGrants = grants
        return configuration
    }
}

@Test @MainActor func folderRepairPersistsCandidateBeforeRequestThenPromotesExactBinding() async throws {
    let fixture = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString, directoryHint: .isDirectory)
    defer { try? FileManager.default.removeItem(at: fixture) }
    let oldRoot = fixture.appending(path: "old", directoryHint: .isDirectory)
    let replacementRoot = fixture.appending(path: "replacement", directoryHint: .isDirectory)
    try FileManager.default.createDirectory(at: oldRoot, withIntermediateDirectories: true)
    try FileManager.default.createDirectory(at: replacementRoot, withIntermediateDirectories: true)
    let offer = UUID()
    let peer = UUID()
    let folder = UUID()
    let replacement = try SelectedDirectoryGrant.capture(url: replacementRoot, purpose: .folderSync)
    let persistence = AppleAppPersistence(directoryURL: fixture.appending(path: "state", directoryHint: .isDirectory))

    let sequence = RequestSequence()
    let recorder = RequestRecorder(removeAfterRequest: false) { request in
        switch sequence.next() {
        case 0:
            #expect(request.url?.path == "/api/v1/sync/status")
            return TestResponse.response(request, status: 200, json: folderStatusJSON(
                offer: offer, folder: folder, peer: peer, issue: "folderAccess"
            ))
        case 1:
            #expect(request.url?.path == "/api/v1/sync/repair")
            let saved = try awaitValue { try await persistence.loadPendingFolderRepairs() }
            #expect(saved.count == 1)
            #expect(saved.first?.offerId == offer)
            let payload = try #require(folderRepairRequestBody(request))
            let object = try #require(JSONSerialization.jsonObject(with: payload) as? [String: Any])
            #expect(UUID(uuidString: try #require(object["offerId"] as? String)) == offer)
            #expect((object["selectedRoot"] as? String)?.hasSuffix("/replacement") == true)
            return TestResponse.response(request, status: 200, json: mutationJSON(offer: offer))
        case 2:
            #expect(request.url?.path == "/api/v1/sync/status")
            return TestResponse.response(request, status: 200, json: folderStatusJSON(
                offer: offer, folder: folder, peer: peer, issue: nil
            ))
        default:
            Issue.record("Unexpected folder repair request")
            return TestResponse.response(request, status: 500, json: "{}")
        }
    }
    let port = RecordingURLProtocol.recorder.install(recorder)
    let configuration = try NodeConnectionConfiguration(
        baseURL: URL(string: "http://127.0.0.1:\(port)")!,
        apiToken: String(repeating: "r", count: 32)
    )
    let sessionConfiguration = URLSessionConfiguration.ephemeral
    sessionConfiguration.protocolClasses = [RecordingURLProtocol.self]
    let bootstrapper = FolderGrantBootstrapper(configuration: configuration)
    let model = CovalentAppModel(
        persistence: persistence,
        client: NodeClient(configuration: configuration, session: URLSession(configuration: sessionConfiguration)),
        configuration: configuration,
        localNodeBootstrapper: bootstrapper
    )

    let oldGrantResult = await model.addDirectoryGrant(
        url: oldRoot,
        purpose: DirectoryAccessPurpose.folderSync
    )
    _ = try #require(oldGrantResult)
    await model.refreshFolders()
    let repaired = await model.repairFolderAccess(offerId: offer, grant: replacement)
    #expect(repaired)
    #expect(bootstrapper.pendingRepairRestartCalls == 1)
    #expect(bootstrapper.restartCalls == 1)
    #expect(bootstrapper.lastPendingRepairGrants.count == 1)
    #expect(bootstrapper.lastPendingRepairGrants.first?.folderOfferId == offer)
    #expect(model.directoryGrants.count == 1)
    #expect(model.directoryGrants.first?.id == replacement.id)
    #expect(model.directoryGrants.first?.folderOfferId == offer)
    #expect(model.pendingFolderRepairs.isEmpty)
    #expect(try await persistence.loadPendingFolderRepairs().isEmpty)
}

@Test @MainActor func uncertainFolderRepairRetainsExactPendingChoiceForRetry() async throws {
    let fixture = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString, directoryHint: .isDirectory)
    defer { try? FileManager.default.removeItem(at: fixture) }
    let oldRoot = fixture.appending(path: "old", directoryHint: .isDirectory)
    let replacementRoot = fixture.appending(path: "replacement", directoryHint: .isDirectory)
    try FileManager.default.createDirectory(at: oldRoot, withIntermediateDirectories: true)
    try FileManager.default.createDirectory(at: replacementRoot, withIntermediateDirectories: true)
    let offer = UUID()
    let peer = UUID()
    let folder = UUID()
    let replacement = try SelectedDirectoryGrant.capture(url: replacementRoot, purpose: .folderSync)
    let persistence = AppleAppPersistence(directoryURL: fixture.appending(path: "state", directoryHint: .isDirectory))
    let sequence = RequestSequence()
    let requestRoots = FolderRepairRequestRoots()
    let recorder = RequestRecorder(removeAfterRequest: false) { request in
        switch sequence.next() {
        case 0:
            return TestResponse.response(request, status: 200, json: folderStatusJSON(
                offer: offer, folder: folder, peer: peer, issue: "folderAccess"
            ))
        case 1:
            #expect(request.url?.path == "/api/v1/sync/repair")
            requestRoots.append(try selectedRoot(from: request))
            throw URLError(.networkConnectionLost)
        case 2:
            #expect(request.url?.path == "/api/v1/sync/repair")
            requestRoots.append(try selectedRoot(from: request))
            return TestResponse.response(request, status: 200, json: mutationJSON(offer: offer))
        case 3:
            return TestResponse.response(request, status: 200, json: folderStatusJSON(
                offer: offer, folder: folder, peer: peer, issue: nil
            ))
        default:
            Issue.record("Unexpected folder repair retry request")
            return TestResponse.response(request, status: 500, json: "{}")
        }
    }
    let port = RecordingURLProtocol.recorder.install(recorder)
    let configuration = try NodeConnectionConfiguration(
        baseURL: URL(string: "http://127.0.0.1:\(port)")!,
        apiToken: String(repeating: "u", count: 32)
    )
    let sessionConfiguration = URLSessionConfiguration.ephemeral
    sessionConfiguration.protocolClasses = [RecordingURLProtocol.self]
    let bootstrapper = FolderGrantBootstrapper(configuration: configuration)
    let model = CovalentAppModel(
        persistence: persistence,
        client: NodeClient(configuration: configuration, session: URLSession(configuration: sessionConfiguration)),
        configuration: configuration,
        localNodeBootstrapper: bootstrapper
    )
    let oldGrantResult = await model.addDirectoryGrant(
        url: oldRoot,
        purpose: DirectoryAccessPurpose.folderSync
    )
    let oldGrant = try #require(oldGrantResult)
    await model.refreshFolders()

    #expect(!(await model.repairFolderAccess(offerId: offer, grant: replacement)))
    #expect(model.directoryGrants == [oldGrant])
    let pending = try #require(model.pendingFolderRepairs.first)
    #expect(pending.offerId == offer)
    #expect(pending.replacementGrant.id == replacement.id)
    #expect((await model.retryFolderAccessRepair(offerId: offer)))
    #expect(requestRoots.values.count == 2)
    #expect(requestRoots.values[0] == requestRoots.values[1])
    #expect(model.pendingFolderRepairs.isEmpty)
    #expect(model.directoryGrants.first?.id == replacement.id)
}

@Test @MainActor func ambiguousLegacyFolderRepairMakesNoDurableOrNodeMutation() async throws {
    let fixture = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString, directoryHint: .isDirectory)
    defer { try? FileManager.default.removeItem(at: fixture) }
    let roots = ["first", "second", "replacement"].map {
        fixture.appending(path: $0, directoryHint: .isDirectory)
    }
    for root in roots {
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
    }
    let offer = UUID()
    let secondOffer = UUID()
    let peer = UUID()
    let secondPeer = UUID()
    let folder = UUID()
    let secondFolder = UUID()
    let persistence = AppleAppPersistence(directoryURL: fixture.appending(path: "state", directoryHint: .isDirectory))
    let sequence = RequestSequence()
    let recorder = RequestRecorder(removeAfterRequest: false) { request in
        #expect(sequence.next() == 0)
        #expect(request.url?.path == "/api/v1/sync/status")
        let shares = [
            folderShareJSON(offer: offer, folder: folder, peer: peer, label: "Plans"),
            folderShareJSON(offer: secondOffer, folder: secondFolder, peer: secondPeer, label: "Photos"),
        ].joined(separator: ",")
        return TestResponse.response(
            request,
            status: 200,
            json: "{\"schemaVersion\":1,\"availability\":\"unavailable\",\"lifecycle\":\"stopped\",\"issue\":\"folderAccess\",\"healthFreshness\":\"unknown\",\"connectionFreshness\":\"unknown\",\"peers\":[],\"shares\":[\(shares)],\"folders\":[]}"
        )
    }
    let port = RecordingURLProtocol.recorder.install(recorder)
    let configuration = try NodeConnectionConfiguration(
        baseURL: URL(string: "http://127.0.0.1:\(port)")!,
        apiToken: String(repeating: "a", count: 32)
    )
    let sessionConfiguration = URLSessionConfiguration.ephemeral
    sessionConfiguration.protocolClasses = [RecordingURLProtocol.self]
    let bootstrapper = FolderGrantBootstrapper(configuration: configuration)
    let model = CovalentAppModel(
        persistence: persistence,
        client: NodeClient(configuration: configuration, session: URLSession(configuration: sessionConfiguration)),
        configuration: configuration,
        localNodeBootstrapper: bootstrapper
    )
    for root in roots.prefix(2) {
        let grant = await model.addDirectoryGrant(url: root, purpose: .folderSync)
        #expect(grant != nil)
    }
    await model.refreshFolders()
    let replacement = try SelectedDirectoryGrant.capture(url: roots[2], purpose: .folderSync)

    #expect(!(await model.repairFolderAccess(offerId: offer, grant: replacement)))
    #expect(sequence.count == 1)
    #expect(bootstrapper.pendingRepairRestartCalls == 0)
    #expect(model.pendingFolderRepairs.isEmpty)
    #expect(model.directoryGrants.count == 2)
    #expect(try await persistence.loadPendingFolderRepairs().isEmpty)
}

private func folderStatusJSON(
    offer: UUID,
    folder: UUID,
    peer: UUID,
    issue: String?
) -> String {
    let issueJSON = issue.map { "\"\($0)\"" } ?? "null"
    return "{\"schemaVersion\":1,\"availability\":\"available\",\"lifecycle\":\"needsAttention\",\"issue\":\(issueJSON),\"healthFreshness\":\"unknown\",\"connectionFreshness\":\"fresh\",\"peers\":[{\"peerId\":\"\(peer.uuidString.lowercased())\",\"displayName\":\"Peer Mac\"}],\"shares\":[{\"offerId\":\"\(offer.uuidString.lowercased())\",\"folderId\":\"\(folder.uuidString.lowercased())\",\"label\":\"Plans\",\"peerId\":\"\(peer.uuidString.lowercased())\",\"incoming\":true,\"phase\":\"ready\",\"expiresAtUnixMs\":null,\"expired\":false,\"peerConnection\":\"connected\"}],\"folders\":[]}"
}

@Test @MainActor func lostRenewalResponseIsRecoveredByStatusWithoutRestartingWorker() async throws {
    let fixture = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
    defer { try? FileManager.default.removeItem(at: fixture) }
    let root = fixture.appending(path: "source")
    try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
    let grant = try SelectedDirectoryGrant.capture(url: root, purpose: .folderSync)
    let oldID = UUID()
    let newID = UUID()
    let folder = UUID()
    let peer = UUID()
    let persistence = AppleAppPersistence(directoryURL: fixture.appending(path: "state"))
    let sequence = RequestSequence()
    let recorder = RequestRecorder(removeAfterRequest: false) { request in
        switch sequence.next() {
        case 0:
            #expect(request.url?.path == "/api/v1/sync/folders")
            return TestResponse.response(request, status: 200, json: mutationJSON(offer: oldID))
        case 1:
            return TestResponse.response(request, status: 200, json: renewalModelStatus(
                offer: oldID, folder: folder, peer: peer, incoming: false, expired: true
            ))
        case 2:
            #expect(request.url?.path == "/api/v1/sync/renew")
            throw URLError(.networkConnectionLost)
        case 3:
            return TestResponse.response(request, status: 200, json: renewalModelStatus(
                offer: newID, folder: folder, peer: peer, incoming: false, superseded: oldID
            ))
        default:
            Issue.record("Unexpected renewal request")
            return TestResponse.response(request, status: 500, json: "{}")
        }
    }
    let (model, bootstrapper) = try renewalModel(recorder: recorder, persistence: persistence)
    #expect(await model.offerFolder(peerId: peer, folderId: folder, label: "Plans", grant: grant))
    #expect(!(await model.renewFolderInvitation(oldID)))
    #expect(model.directoryGrants.first?.folderOfferId == oldID)
    await model.refreshFolders()
    #expect(model.directoryGrants.first?.folderOfferId == newID)
    #expect(model.directoryGrants.first?.bookmarkData == grant.bookmarkData)
    let reopened = try #require(try await persistence.loadDirectoryGrants().first)
    #expect(reopened.folderOfferId == newID)
    #expect(reopened.id == grant.id)
    #expect(reopened.bookmarkData == grant.bookmarkData)
    #expect(bootstrapper.restartCalls == 1)
    #expect(!model.folderSyncMutationInFlight)
}

@Test @MainActor func incomingRenewalRetriesScopeRetirementBeforeFreshSelectionOfPreviousFolder() async throws {
    let fixture = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
    defer { try? FileManager.default.removeItem(at: fixture) }
    let root = fixture.appending(path: "destination")
    try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
    let oldGrant = try SelectedDirectoryGrant.capture(url: root, purpose: .folderSync)
    let freshGrant = try SelectedDirectoryGrant.capture(url: root, purpose: .folderSync)
    let oldID = UUID()
    let newID = UUID()
    let folder = UUID()
    let peer = UUID()
    let persistence = AppleAppPersistence(directoryURL: fixture.appending(path: "state"))
    let sequence = RequestSequence()
    let recorder = RequestRecorder(removeAfterRequest: false) { request in
        switch sequence.next() {
        case 0:
            return TestResponse.response(request, status: 200, json: mutationJSON(offer: oldID))
        case 1:
            return TestResponse.response(request, status: 200, json: renewalModelStatus(
                offer: oldID, folder: folder, peer: peer, incoming: true, phase: "awaitingCommit"
            ))
        case 2, 3:
            return TestResponse.response(request, status: 200, json: renewalModelStatus(
                offer: newID, folder: folder, peer: peer, incoming: true, superseded: oldID
            ))
        case 4:
            #expect(request.url?.path == "/api/v1/sync/accept")
            let payload = try #require(folderRepairRequestBody(request))
            let object = try #require(JSONSerialization.jsonObject(with: payload) as? [String: Any])
            #expect(UUID(uuidString: try #require(object["offerId"] as? String)) == newID)
            return TestResponse.response(request, status: 200, json: mutationJSON(offer: newID))
        case 5:
            return TestResponse.response(request, status: 200, json: renewalModelStatus(
                offer: newID, folder: folder, peer: peer, incoming: true,
                phase: "awaitingCommit", superseded: oldID
            ))
        default:
            Issue.record("Unexpected recipient renewal request")
            return TestResponse.response(request, status: 500, json: "{}")
        }
    }
    let (model, bootstrapper) = try renewalModel(recorder: recorder, persistence: persistence)
    #expect(await model.acceptFolder(offerId: oldID, grant: oldGrant))
    bootstrapper.restartFailuresRemaining = 1
    await model.refreshFolders()
    #expect(model.directoryGrants.isEmpty)
    #expect(try await persistence.loadDirectoryGrants().isEmpty)
    #expect(model.folderSyncStatus == nil)
    #expect(bootstrapper.restartCalls == 2)
    await model.refreshFolders()
    #expect(bootstrapper.restartCalls == 3)
    #expect(model.folderSyncStatus?.shares.first?.offerId == newID)
    #expect(await model.acceptFolder(offerId: newID, grant: freshGrant))
    #expect(model.directoryGrants.first?.id == freshGrant.id)
    #expect(model.directoryGrants.first?.folderOfferId == newID)
    #expect(FileManager.default.fileExists(atPath: root.path))
}

@Test @MainActor func remoteRemovalRetriesScopeRetirementAndPreservesFiles() async throws {
    try await removalRetirementJourney(localRemoval: false)
}

@Test @MainActor func localRemovalRetriesFailedScopeRetirementOnNextRefresh() async throws {
    try await removalRetirementJourney(localRemoval: true)
}

@Test @MainActor func remoteRemovalRetiresAPendingRepairAfterItsResponseWasLost() async throws {
    try await removalRetirementJourney(localRemoval: false, pendingRepair: true)
}

@MainActor private func removalRetirementJourney(localRemoval: Bool, pendingRepair: Bool = false) async throws {
    let fixture = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
    defer { try? FileManager.default.removeItem(at: fixture) }
    let root = fixture.appending(path: "destination")
    try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
    let file = root.appending(path: "keep.txt")
    let bytes = Data("Keep this file after removal".utf8)
    try bytes.write(to: file)
    let grant = try SelectedDirectoryGrant.capture(url: root, purpose: .folderSync)
    let offer = UUID(), folder = UUID(), peer = UUID()
    let persistence = AppleAppPersistence(directoryURL: fixture.appending(path: "state"))
    let sequence = RequestSequence()
    let removalStep = pendingRepair ? 3 : 2
    let recorder = RequestRecorder(removeAfterRequest: false) { request in
        let step = sequence.next()
        switch step {
        case 0:
            #expect(request.url?.path == "/api/v1/sync/accept")
            return TestResponse.response(request, status: 200, json: mutationJSON(offer: offer))
        case 1:
            if pendingRepair {
                return TestResponse.response(request, status: 200, json: folderStatusJSON(
                    offer: offer, folder: folder, peer: peer, issue: "folderAccess"
                ))
            }
            return TestResponse.response(request, status: 200, json: renewalModelStatus(
                offer: offer, folder: folder, peer: peer, incoming: true, phase: "ready"
            ))
        case 2 where pendingRepair:
            #expect(request.url?.path == "/api/v1/sync/repair")
            throw URLError(.networkConnectionLost)
        case _ where localRemoval && step == removalStep:
            #expect(request.url?.path == "/api/v1/sync/remove")
            return TestResponse.response(request, status: 200, json: mutationJSON(offer: offer))
        case _ where step == removalStep || step == removalStep + 1:
            #expect(request.url?.path == "/api/v1/sync/status")
            return TestResponse.response(request, status: 200, json: renewalModelStatus(
                offer: offer, folder: folder, peer: peer, incoming: true, phase: "removed"
            ))
        default:
            Issue.record("Unexpected removal request")
            return TestResponse.response(request, status: 500, json: "{}")
        }
    }
    let (model, bootstrapper) = try renewalModel(recorder: recorder, persistence: persistence)
    #expect(await model.acceptFolder(offerId: offer, grant: grant))
    if pendingRepair {
        let replacementRoot = fixture.appending(path: "replacement")
        try FileManager.default.createDirectory(at: replacementRoot, withIntermediateDirectories: true)
        let replacement = try SelectedDirectoryGrant.capture(url: replacementRoot, purpose: .folderSync)
        #expect(!(await model.repairFolderAccess(offerId: offer, grant: replacement)))
        #expect(model.pendingFolderRepairs.count == 1)
        #expect(bootstrapper.pendingRepairRestartCalls == 1)
    }
    bootstrapper.restartFailuresRemaining = 1
    if localRemoval { await model.removeFolder(offer) }
    else { await model.refreshFolders() }
    #expect(model.directoryGrants.isEmpty)
    #expect(try await persistence.loadDirectoryGrants().isEmpty)
    #expect(model.pendingFolderRepairs.isEmpty)
    #expect(try await persistence.loadPendingFolderRepairs().isEmpty)
    #expect(bootstrapper.restartCalls == 2)
    await model.refreshFolders()
    #expect(bootstrapper.restartCalls == 3)
    #expect(model.folderSyncStatus?.shares.first?.phase == .removed)
    #expect(!model.folderSyncMutationInFlight)
    #expect(try Data(contentsOf: file) == bytes)
}

@MainActor private func renewalModel(
    recorder: RequestRecorder, persistence: AppleAppPersistence
) throws -> (CovalentAppModel, FolderGrantBootstrapper) {
    let port = RecordingURLProtocol.recorder.install(recorder)
    let configuration = try NodeConnectionConfiguration(
        baseURL: URL(string: "http://127.0.0.1:\(port)")!, apiToken: String(repeating: "n", count: 32)
    )
    let sessionConfiguration = URLSessionConfiguration.ephemeral
    sessionConfiguration.protocolClasses = [RecordingURLProtocol.self]
    let bootstrapper = FolderGrantBootstrapper(configuration: configuration)
    return (CovalentAppModel(
        persistence: persistence,
        client: NodeClient(configuration: configuration, session: URLSession(configuration: sessionConfiguration)),
        configuration: configuration, localNodeBootstrapper: bootstrapper
    ), bootstrapper)
}

private func renewalModelStatus(
    offer: UUID, folder: UUID, peer: UUID, incoming: Bool, phase: String = "offered",
    expired: Bool = false, superseded: UUID? = nil
) -> String {
    let oldIDs = superseded.map { "\"\($0.uuidString)\"" } ?? ""
    return """
    {"availability":"available","lifecycle":"running","issue":null,"healthFreshness":"fresh","peers":[],
    "shares":[{"offerId":"\(offer)","folderId":"\(folder)","peerId":"\(peer)","label":"Plans",
    "incoming":\(incoming),"phase":"\(phase)","expiresAtUnixMs":100,"expired":\(expired),
    "supersededOfferIds":[\(oldIDs)]}],"folders":[]}
    """
}

private func mutationJSON(offer: UUID) -> String {
    "{\"offerId\":\"\(offer.uuidString.lowercased())\",\"lifecycle\":\"stopped\",\"issue\":\"folderAccess\"}"
}

private func folderShareJSON(
    offer: UUID,
    folder: UUID,
    peer: UUID,
    label: String
) -> String {
    "{\"offerId\":\"\(offer.uuidString.lowercased())\",\"folderId\":\"\(folder.uuidString.lowercased())\",\"label\":\"\(label)\",\"peerId\":\"\(peer.uuidString.lowercased())\",\"incoming\":true,\"phase\":\"ready\",\"expiresAtUnixMs\":null,\"expired\":false,\"peerConnection\":\"unknown\"}"
}

private func folderRepairRequestBody(_ request: URLRequest) -> Data? {
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

private func selectedRoot(from request: URLRequest) throws -> String {
    let payload = try #require(folderRepairRequestBody(request))
    let object = try #require(JSONSerialization.jsonObject(with: payload) as? [String: Any])
    return try #require(object["selectedRoot"] as? String)
}

private final class FolderRepairRequestRoots: @unchecked Sendable {
    private let lock = NSLock()
    private var stored: [String] = []
    var values: [String] { lock.withLock { stored } }
    func append(_ value: String) { lock.withLock { stored.append(value) } }
}

/// Bridges the actor read needed inside the synchronous URLProtocol recorder.
private func awaitValue<Value: Sendable>(
    _ operation: @escaping @Sendable () async throws -> Value
) throws -> Value {
    let semaphore = DispatchSemaphore(value: 0)
    let box = FolderRepairResultBox<Value>()
    Task {
        do { box.result = .success(try await operation()) }
        catch { box.result = .failure(error) }
        semaphore.signal()
    }
    semaphore.wait()
    return try #require(box.result).get()
}

private final class FolderRepairResultBox<Value: Sendable>: @unchecked Sendable {
    private let lock = NSLock()
    private var stored: Result<Value, Error>?
    var result: Result<Value, Error>? {
        get { lock.withLock { stored } }
        set { lock.withLock { stored = newValue } }
    }
}

private enum FolderGrantTestError: Error { case restartFailed }

@Test @MainActor func folderOfferRetriesTypedBusyInPlaceWithoutRestartingAgain() async throws {
    let fixture = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
    defer { try? FileManager.default.removeItem(at: fixture) }
    let source = fixture.appending(path: "source")
    try FileManager.default.createDirectory(at: source, withIntermediateDirectories: true)
    let grant = try SelectedDirectoryGrant.capture(url: source, purpose: .folderSync)
    let offer = UUID()
    let sequence = RequestSequence()
    let bodies = FolderRepairRequestRoots()
    let recorder = RequestRecorder(removeAfterRequest: false) { request in
        switch sequence.next() {
        case 0:
            #expect(request.url?.path == "/api/v1/sync/folders")
            bodies.append(String(decoding: try #require(folderRepairRequestBody(request)), as: UTF8.self))
            return TestResponse.response(
                request,
                status: 503,
                json: #"{"protocolVersion":1,"code":"folder_sync_busy","message":"scan","retryable":true}"#
            )
        case 1:
            #expect(request.url?.path == "/api/v1/sync/folders")
            bodies.append(String(decoding: try #require(folderRepairRequestBody(request)), as: UTF8.self))
            return TestResponse.response(
                request,
                status: 200,
                json: "{\"offerId\":\"\(offer.uuidString)\",\"lifecycle\":\"running\",\"issue\":null}"
            )
        case 2:
            return TestResponse.response(
                request,
                status: 200,
                json: #"{"availability":"available","lifecycle":"running","issue":null,"healthFreshness":"fresh","peers":[],"shares":[],"folders":[]}"#
            )
        default:
            Issue.record("Unexpected in-place offer retry")
            return TestResponse.response(request, status: 500, json: "{}")
        }
    }
    let (model, bootstrapper) = try folderMutationModel(
        recorder: recorder, persistence: AppleAppPersistence(directoryURL: fixture.appending(path: "state")))

    #expect(await model.offerFolder(peerId: UUID(), folderId: UUID(), label: "Plans", grant: grant))
    #expect(sequence.count == 3)
    #expect(bodies.values.count == 2)
    #expect(bodies.values[0] == bodies.values[1])
    #expect(bootstrapper.restartCalls == 1)
}

@Test @MainActor func folderAcceptRetriesTypedBusyInPlaceWithoutRestartingAgain() async throws {
    let fixture = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
    defer { try? FileManager.default.removeItem(at: fixture) }
    let destination = fixture.appending(path: "destination")
    try FileManager.default.createDirectory(at: destination, withIntermediateDirectories: true)
    let grant = try SelectedDirectoryGrant.capture(url: destination, purpose: .folderSync)
    let offer = UUID()
    let folder = UUID()
    let peer = UUID()
    let sequence = RequestSequence()
    let bodies = FolderRepairRequestRoots()
    let recorder = RequestRecorder(removeAfterRequest: false) { request in
        switch sequence.next() {
        case 0:
            #expect(request.url?.path == "/api/v1/sync/accept")
            bodies.append(String(decoding: try #require(folderRepairRequestBody(request)), as: UTF8.self))
            return TestResponse.response(
                request,
                status: 503,
                json: #"{"protocolVersion":1,"code":"folder_sync_busy","message":"scan","retryable":true}"#
            )
        case 1:
            #expect(request.url?.path == "/api/v1/sync/accept")
            bodies.append(String(decoding: try #require(folderRepairRequestBody(request)), as: UTF8.self))
            return TestResponse.response(request, status: 200, json: mutationJSON(offer: offer))
        case 2:
            return TestResponse.response(
                request,
                status: 200,
                json: folderStatusJSON(offer: offer, folder: folder, peer: peer, issue: nil)
            )
        default:
            Issue.record("Unexpected in-place accept retry")
            return TestResponse.response(request, status: 500, json: "{}")
        }
    }
    let (model, bootstrapper) = try folderMutationModel(
        recorder: recorder, persistence: AppleAppPersistence(directoryURL: fixture.appending(path: "state")))

    #expect(await model.acceptFolder(offerId: offer, grant: grant))
    #expect(sequence.count == 3)
    #expect(bodies.values.count == 2)
    #expect(bodies.values[0] == bodies.values[1])
    #expect(bootstrapper.restartCalls == 1)
}

@Test @MainActor func otherRetryableOfferFailureInvalidatesCachedLaunchBeforeAlertRetry() async throws {
    let fixture = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
    defer { try? FileManager.default.removeItem(at: fixture) }
    let source = fixture.appending(path: "source")
    try FileManager.default.createDirectory(at: source, withIntermediateDirectories: true)
    let grant = try SelectedDirectoryGrant.capture(url: source, purpose: .folderSync)
    let peer = UUID()
    let folder = UUID()
    let offer = UUID()
    let sequence = RequestSequence()
    let recorder = RequestRecorder(removeAfterRequest: false) { request in
        switch sequence.next() {
        case 0:
            return TestResponse.response(
                request,
                status: 409,
                json: #"{"protocolVersion":1,"code":"folder_offer_rejected","message":"retry manually","retryable":true}"#
            )
        case 1:
            return TestResponse.response(
                request,
                status: 200,
                json: "{\"offerId\":\"\(offer.uuidString)\",\"lifecycle\":\"running\",\"issue\":null}"
            )
        case 2:
            return TestResponse.response(
                request,
                status: 200,
                json: #"{"availability":"available","lifecycle":"running","issue":null,"healthFreshness":"fresh","peers":[],"shares":[],"folders":[]}"#
            )
        default:
            Issue.record("Unexpected explicit offer retry")
            return TestResponse.response(request, status: 500, json: "{}")
        }
    }
    let (model, bootstrapper) = try folderMutationModel(
        recorder: recorder, persistence: AppleAppPersistence(directoryURL: fixture.appending(path: "state")))

    #expect(!(await model.offerFolder(peerId: peer, folderId: folder, label: "Plans", grant: grant)))
    #expect(sequence.count == 1)
    let retry = try #require(model.takeAlertRecovery())
    await retry()
    #expect(sequence.count == 3)
    #expect(bootstrapper.restartCalls == 2)
}

@Test @MainActor func otherRetryableAcceptFailureInvalidatesCachedLaunchBeforeAlertRetry() async throws {
    let fixture = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
    defer { try? FileManager.default.removeItem(at: fixture) }
    let destination = fixture.appending(path: "destination")
    try FileManager.default.createDirectory(at: destination, withIntermediateDirectories: true)
    let grant = try SelectedDirectoryGrant.capture(url: destination, purpose: .folderSync)
    let offer = UUID()
    let folder = UUID()
    let peer = UUID()
    let sequence = RequestSequence()
    let recorder = RequestRecorder(removeAfterRequest: false) { request in
        switch sequence.next() {
        case 0:
            return TestResponse.response(
                request,
                status: 503,
                json: #"{"protocolVersion":1,"code":"folder_topology_busy","message":"retry manually","retryable":true}"#
            )
        case 1:
            return TestResponse.response(request, status: 200, json: mutationJSON(offer: offer))
        case 2:
            return TestResponse.response(
                request,
                status: 200,
                json: folderStatusJSON(offer: offer, folder: folder, peer: peer, issue: nil)
            )
        default:
            Issue.record("Unexpected explicit accept retry")
            return TestResponse.response(request, status: 500, json: "{}")
        }
    }
    let (model, bootstrapper) = try folderMutationModel(
        recorder: recorder, persistence: AppleAppPersistence(directoryURL: fixture.appending(path: "state")))

    #expect(!(await model.acceptFolder(offerId: offer, grant: grant)))
    #expect(sequence.count == 1)
    let retry = try #require(model.takeAlertRecovery())
    await retry()
    #expect(sequence.count == 3)
    #expect(bootstrapper.restartCalls == 2)
}

@Test @MainActor func identicalGrantRetryRevalidatesLiveFolderServiceBeforeSkippingRestart() async throws {
    let fixture = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
    defer { try? FileManager.default.removeItem(at: fixture) }
    let source = fixture.appending(path: "source")
    try FileManager.default.createDirectory(at: source, withIntermediateDirectories: true)
    let grant = try SelectedDirectoryGrant.capture(url: source, purpose: .folderSync)
    let offer = UUID()
    let peer = UUID()
    let folder = UUID()
    let sequence = RequestSequence()
    let recorder = RequestRecorder(removeAfterRequest: false) { request in
        switch sequence.next() {
        case 0, 3:
            #expect(request.url?.path == "/api/v1/sync/folders")
            return TestResponse.response(
                request,
                status: 200,
                json: "{\"offerId\":\"\(offer.uuidString)\",\"lifecycle\":\"running\",\"issue\":null}"
            )
        case 1, 2, 4:
            #expect(request.url?.path == "/api/v1/sync/status")
            return TestResponse.response(
                request,
                status: 200,
                json: #"{"availability":"available","lifecycle":"running","issue":null,"healthFreshness":"fresh","peers":[],"shares":[],"folders":[]}"#
            )
        default:
            Issue.record("Unexpected identical-grant retry request")
            return TestResponse.response(request, status: 500, json: "{}")
        }
    }
    let (model, bootstrapper) = try folderMutationModel(
        recorder: recorder, persistence: AppleAppPersistence(directoryURL: fixture.appending(path: "state")))

    #expect(await model.offerFolder(peerId: peer, folderId: folder, label: "Plans", grant: grant))
    #expect(await model.offerFolder(peerId: peer, folderId: folder, label: "Plans", grant: grant))
    #expect(sequence.count == 5)
    #expect(bootstrapper.restartCalls == 1)
}

@Test @MainActor func unavailableFolderServiceInvalidatesIdenticalGrantLaunchAndRestarts() async throws {
    let fixture = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
    defer { try? FileManager.default.removeItem(at: fixture) }
    let source = fixture.appending(path: "source")
    try FileManager.default.createDirectory(at: source, withIntermediateDirectories: true)
    let grant = try SelectedDirectoryGrant.capture(url: source, purpose: .folderSync)
    let offer = UUID()
    let peer = UUID()
    let folder = UUID()
    let sequence = RequestSequence()
    let recorder = RequestRecorder(removeAfterRequest: false) { request in
        switch sequence.next() {
        case 0, 3:
            #expect(request.url?.path == "/api/v1/sync/folders")
            return TestResponse.response(
                request,
                status: 200,
                json: "{\"offerId\":\"\(offer.uuidString)\",\"lifecycle\":\"running\",\"issue\":null}"
            )
        case 1, 4:
            #expect(request.url?.path == "/api/v1/sync/status")
            return TestResponse.response(
                request,
                status: 200,
                json: #"{"availability":"available","lifecycle":"running","issue":null,"healthFreshness":"fresh","peers":[],"shares":[],"folders":[]}"#
            )
        case 2:
            #expect(request.url?.path == "/api/v1/sync/status")
            return TestResponse.response(
                request,
                status: 200,
                json: #"{"availability":"unavailable","lifecycle":"stopped","issue":"folderAccess","healthFreshness":"unknown","connectionFreshness":"unknown","peers":[],"shares":[],"folders":[]}"#
            )
        default:
            Issue.record("Unexpected unavailable-service retry request")
            return TestResponse.response(request, status: 500, json: "{}")
        }
    }
    let (model, bootstrapper) = try folderMutationModel(
        recorder: recorder, persistence: AppleAppPersistence(directoryURL: fixture.appending(path: "state")))

    #expect(await model.offerFolder(peerId: peer, folderId: folder, label: "Plans", grant: grant))
    #expect(await model.offerFolder(peerId: peer, folderId: folder, label: "Plans", grant: grant))
    #expect(sequence.count == 5)
    #expect(bootstrapper.restartCalls == 2)
}

@MainActor
private func folderMutationModel(
    recorder: RequestRecorder,
    persistence: AppleAppPersistence
) throws -> (CovalentAppModel, FolderGrantBootstrapper) {
    let port = RecordingURLProtocol.recorder.install(recorder)
    let configuration = try NodeConnectionConfiguration(
        baseURL: URL(string: "http://127.0.0.1:\(port)")!,
        apiToken: String(repeating: "m", count: 32)
    )
    let sessionConfiguration = URLSessionConfiguration.ephemeral
    sessionConfiguration.protocolClasses = [RecordingURLProtocol.self]
    let bootstrapper = FolderGrantBootstrapper(configuration: configuration)
    return (
        CovalentAppModel(
            persistence: persistence,
            client: NodeClient(
                configuration: configuration,
                session: URLSession(configuration: sessionConfiguration)
            ),
            configuration: configuration,
            localNodeBootstrapper: bootstrapper
        ),
        bootstrapper
    )
}

@Test @MainActor func folderGrantIsNotRetainedInMemoryWhenDurableSaveFails() async throws {
    let source = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString, directoryHint: .isDirectory)
    defer { try? FileManager.default.removeItem(at: source) }
    try FileManager.default.createDirectory(at: source, withIntermediateDirectories: true)
    let grant = try SelectedDirectoryGrant.capture(url: source, purpose: .folderSync)
    let persistence = AppleAppPersistence(directoryURL: URL(fileURLWithPath: "/dev/null"))
    let model = CovalentAppModel(
        persistence: persistence,
        configuration: .localDefault
    )

    let offered = await model.offerFolder(peerId: UUID(), folderId: UUID(), label: "Plans", grant: grant)
    #expect(!offered)
    #expect(model.directoryGrants.isEmpty)
}

@Test @MainActor func folderOfferRetriesHostRestartAfterTheGrantWasAlreadySaved() async throws {
    let root = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString, directoryHint: .isDirectory)
    defer { try? FileManager.default.removeItem(at: root) }
    try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
    let source = root.appending(path: "source", directoryHint: .isDirectory)
    try FileManager.default.createDirectory(at: source, withIntermediateDirectories: true)
    let persistence = AppleAppPersistence(directoryURL: root.appending(path: "state", directoryHint: .isDirectory))
    let peer = UUID()
    let folder = UUID()
    let offer = UUID()
    let sequence = RequestSequence()
    let recorder = RequestRecorder(removeAfterRequest: false) { request in
        switch sequence.next() {
        case 0:
            #expect(request.url?.path == "/api/v1/sync/folders")
            return TestResponse.response(request, status: 200, json: "{\"offerId\":\"\(offer.uuidString.lowercased())\",\"lifecycle\":\"running\",\"issue\":null}")
        case 1:
            #expect(request.url?.path == "/api/v1/sync/status")
            return TestResponse.response(request, status: 200, json: "{\"schemaVersion\":1,\"availability\":\"available\",\"lifecycle\":\"running\",\"issue\":null,\"healthFreshness\":\"fresh\",\"peers\":[],\"shares\":[],\"folders\":[]}")
        default:
            Issue.record("Unexpected folder request")
            return TestResponse.response(request, status: 500, json: "{}")
        }
    }
    let port = RecordingURLProtocol.recorder.install(recorder)
    let configuration = try NodeConnectionConfiguration(
        baseURL: URL(string: "http://127.0.0.1:\(port)")!,
        apiToken: String(repeating: "f", count: 32)
    )
    let sessionConfiguration = URLSessionConfiguration.ephemeral
    sessionConfiguration.protocolClasses = [RecordingURLProtocol.self]
    let client = NodeClient(configuration: configuration, session: URLSession(configuration: sessionConfiguration))
    let bootstrapper = FolderGrantBootstrapper(configuration: configuration)
    bootstrapper.restartFailuresRemaining = 1
    let model = CovalentAppModel(
        persistence: persistence,
        client: client,
        configuration: configuration,
        localNodeBootstrapper: bootstrapper
    )
    let captured = try SelectedDirectoryGrant.capture(url: source, purpose: .folderSync)
    // The durable format intentionally uses ISO-8601 second precision.
    let grant = SelectedDirectoryGrant(
        id: captured.id,
        displayName: captured.displayName,
        purpose: captured.purpose,
        bookmarkData: captured.bookmarkData,
        capturedAt: Date(timeIntervalSince1970: 1_788_883_288)
    )

    let firstOffer = await model.offerFolder(peerId: peer, folderId: folder, label: "Plans", grant: grant)
    #expect(!firstOffer)
    let persistedGrants = try await persistence.loadDirectoryGrants()
    #expect(persistedGrants == [grant])

    let retriedOffer = await model.offerFolder(peerId: peer, folderId: folder, label: "Plans", grant: grant)
    #expect(retriedOffer)
    #expect(bootstrapper.restartCalls == 2)
    #expect(try await persistence.loadDirectoryGrants().first?.folderOfferId == offer)
}
