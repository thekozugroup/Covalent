import Foundation
import Testing
@testable import CovalentShared

@Test func newFolderRequestsAlwaysUseAnExplicitOneWayDeletionPolicy() throws {
    let peer = UUID()
    let folder = UUID()
    let defaultRequest = FolderOfferRequest(peerId: peer, folderId: folder, label: "Photos", selectedRoot: "/selected")
    #expect(defaultRequest.linkPolicy == FolderLinkPolicy())
    for propagate in [false, true] {
        for restore in [false, true] {
            let policy = FolderLinkPolicy(propagateSourceDeletions: propagate, restoreLocalDeletions: restore)
            let request = FolderOfferRequest(peerId: peer, folderId: folder, label: "Photos", selectedRoot: "/selected", linkPolicy: policy)
            let payload = try #require(JSONSerialization.jsonObject(with: JSONEncoder().encode(request)) as? [String: Any])
            let selectedPolicy = try #require(payload["linkPolicy"] as? [String: Bool])
            #expect(selectedPolicy == ["propagateSourceDeletions": propagate, "restoreLocalDeletions": restore])
        }
    }
    #expect(throws: (any Error).self) {
        try JSONDecoder().decode(FolderLinkPolicy.self, from: Data("{\"propagateSourceDeletions\":false}".utf8))
    }
}

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

@Test func savedPeerRowsIncludeStatusOnlyPeersAndDeduplicateProviders() throws {
    let providerPeer = UUID()
    let statusOnlyPeer = UUID()
    let provider = ProviderConnection(
        peerId: providerPeer,
        address: "192.0.2.40:8787",
        certificateFingerprint: String(repeating: "a", count: 64)
    )
    let rows = SavedPeerDevice.merge(
        peers: [
            FolderSyncPeer(
                peerId: providerPeer,
                displayName: "Kitchen Mac",
                address: "192.0.2.41:8787"
            ),
            FolderSyncPeer(
                peerId: statusOnlyPeer,
                displayName: "Studio Mac",
                address: "192.0.2.50:8787"
            ),
        ],
        providers: [provider]
    )
    #expect(rows.count == 2)
    #expect(rows.map(\.id) == [providerPeer, statusOnlyPeer])
    #expect(rows[0].peer?.displayName == "Kitchen Mac")
    #expect(rows[0].provider == provider)
    #expect(rows[0].address == "192.0.2.41:8787")
    #expect(rows[1].peer?.displayName == "Studio Mac")
    #expect(rows[1].provider == nil)
    #expect(rows[1].address == "192.0.2.50:8787")
}

@Test @MainActor func statusOnlyPeerAddressRefreshDoesNotRequireProviderRecord() async throws {
    let fixture = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
    defer { try? FileManager.default.removeItem(at: fixture) }
    let peer = UUID()
    let request = PeerAddressRefreshRequest(
        peerId: peer,
        expectedAddress: "192.0.2.10:8787",
        candidateAddress: "192.0.2.11:8787"
    )
    let sequence = RequestSequence()
    let recorder = RequestRecorder(removeAfterRequest: false) { urlRequest in
        switch sequence.next() {
        case 0:
            try expectPeerAddressRequest(urlRequest, equals: request)
            return TestResponse.response(
                urlRequest, status: 200,
                json: #"{"offerId":null,"lifecycle":"running","issue":null}"#
            )
        case 1:
            #expect(urlRequest.url?.path == "/api/v1/sync/status")
            return TestResponse.response(
                urlRequest, status: 200,
                json: peerAddressStatusJSON(peer: peer, address: request.candidateAddress)
            )
        default:
            Issue.record("A status-only peer must not require a provider reload")
            return TestResponse.response(urlRequest, status: 500, json: "{}")
        }
    }
    let model = try peerAddressModel(recorder: recorder, fixture: fixture)
    let outcome = await model.refreshPeerAddress(request)
    guard case let .saved(savedPeer) = outcome else {
        Issue.record("Expected the authenticated status-only peer to save")
        return
    }
    #expect(savedPeer.address == request.candidateAddress)
    #expect(model.folderSyncStatus?.peers.first == savedPeer)
    #expect(model.providers.isEmpty)
    #expect(sequence.count == 2)
}

@Test @MainActor func ambiguousPeerAddressResponseRetainsExactRequestWhenCandidateIsVisible() async throws {
    let fixture = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
    defer { try? FileManager.default.removeItem(at: fixture) }
    let peer = UUID()
    let request = PeerAddressRefreshRequest(
        peerId: peer,
        expectedAddress: "192.0.2.20:8787",
        candidateAddress: "192.0.2.21:8787"
    )
    let sequence = RequestSequence()
    let recorder = RequestRecorder(removeAfterRequest: false) { urlRequest in
        switch sequence.next() {
        case 0:
            try expectPeerAddressRequest(urlRequest, equals: request)
            throw URLError(.networkConnectionLost)
        case 1:
            return TestResponse.response(
                urlRequest, status: 200,
                json: peerAddressStatusJSON(peer: peer, address: request.candidateAddress)
            )
        case 2:
            try expectPeerAddressRequest(urlRequest, equals: request)
            return TestResponse.response(
                urlRequest, status: 200,
                json: #"{"offerId":null,"lifecycle":"running","issue":null}"#
            )
        case 3:
            return TestResponse.response(
                urlRequest, status: 200,
                json: peerAddressStatusJSON(peer: peer, address: request.candidateAddress)
            )
        default:
            Issue.record("Unexpected exact address retry request")
            return TestResponse.response(urlRequest, status: 500, json: "{}")
        }
    }
    let model = try peerAddressModel(recorder: recorder, fixture: fixture)
    let first = await model.refreshPeerAddress(request)
    guard case let .retryExact(retained, failure) = first else {
        Issue.record("Expected an exact retry after an ambiguous response")
        return
    }
    #expect(retained == request)
    #expect(failure.recovery == .retry)
    #expect(failure.summary.contains("visible"))
    let second = await model.refreshPeerAddress(retained)
    guard case let .saved(savedPeer) = second else {
        Issue.record("Expected the exact retry to confirm the saved address")
        return
    }
    #expect(savedPeer.address == request.candidateAddress)
    #expect(sequence.count == 4)
}

@Test func freshPeerAddressRequestRequiresMatchingAuthoritativeStatus() {
    let peerID = UUID()
    let peer = FolderSyncPeer(
        peerId: peerID,
        displayName: "Studio Mac",
        address: "192.0.2.70:8787"
    )
    let matchingStatus = peerAddressStatus(peer: peer)
    #expect(PeerAddressRefreshPolicy.permitsFreshRequest(
        peer: peer,
        status: matchingStatus,
        candidateAddress: "192.0.2.71:8787"
    ))
    #expect(!PeerAddressRefreshPolicy.permitsFreshRequest(
        peer: peer,
        status: nil,
        candidateAddress: "192.0.2.71:8787"
    ))
    #expect(!PeerAddressRefreshPolicy.permitsFreshRequest(
        peer: peer,
        status: peerAddressStatus(peer: FolderSyncPeer(
            peerId: peerID,
            displayName: "Studio Mac",
            address: "192.0.2.72:8787"
        )),
        candidateAddress: "192.0.2.71:8787"
    ))
    #expect(!PeerAddressRefreshPolicy.permitsFreshRequest(
        peer: peer,
        status: peerAddressStatus(peer: FolderSyncPeer(
            peerId: UUID(),
            displayName: "Another Mac",
            address: peer.address
        )),
        candidateAddress: "192.0.2.71:8787"
    ))
}

@Test @MainActor func failedConflictReloadCannotReuseCapturedExpectedAddress() async throws {
    let fixture = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
    defer { try? FileManager.default.removeItem(at: fixture) }
    let peer = FolderSyncPeer(
        peerId: UUID(),
        displayName: "Studio Mac",
        address: "192.0.2.80:8787"
    )
    let request = PeerAddressRefreshRequest(
        peerId: peer.peerId,
        expectedAddress: try #require(peer.address),
        candidateAddress: "192.0.2.81:8787"
    )
    let sequence = RequestSequence()
    let recorder = RequestRecorder(removeAfterRequest: false) { urlRequest in
        switch sequence.next() {
        case 0:
            try expectPeerAddressRequest(urlRequest, equals: request)
            return TestResponse.response(
                urlRequest, status: 409,
                json: #"{"protocolVersion":1,"code":"peer_address_changed","message":"stale","retryable":false}"#
            )
        case 1:
            #expect(urlRequest.url?.path == "/api/v1/sync/status")
            return TestResponse.response(
                urlRequest, status: 503,
                json: #"{"protocolVersion":1,"code":"temporarily_unavailable","message":"retry","retryable":true}"#
            )
        default:
            Issue.record("A failed conflict reload must not send another request")
            return TestResponse.response(urlRequest, status: 500, json: "{}")
        }
    }
    let model = try peerAddressModel(recorder: recorder, fixture: fixture)
    guard case .failed = await model.refreshPeerAddress(request) else {
        Issue.record("Expected the failed authoritative reload to stop this edit")
        return
    }
    #expect(model.folderSyncStatus == nil)
    #expect(!PeerAddressRefreshPolicy.permitsFreshRequest(
        peer: peer,
        status: model.folderSyncStatus,
        candidateAddress: request.candidateAddress
    ))
    #expect(sequence.count == 2)
}

@Test @MainActor func peerAddressConflictReloadsAndRequiresNewConfirmation() async throws {
    let fixture = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
    defer { try? FileManager.default.removeItem(at: fixture) }
    let peer = UUID()
    let stale = PeerAddressRefreshRequest(
        peerId: peer,
        expectedAddress: "192.0.2.30:8787",
        candidateAddress: "192.0.2.31:8787"
    )
    let currentAddress = "192.0.2.32:8787"
    let confirmed = PeerAddressRefreshRequest(
        peerId: peer,
        expectedAddress: currentAddress,
        candidateAddress: stale.candidateAddress
    )
    let sequence = RequestSequence()
    let recorder = RequestRecorder(removeAfterRequest: false) { urlRequest in
        switch sequence.next() {
        case 0:
            try expectPeerAddressRequest(urlRequest, equals: stale)
            return TestResponse.response(
                urlRequest, status: 409,
                json: #"{"protocolVersion":1,"code":"peer_address_changed","message":"stale","retryable":false}"#
            )
        case 1:
            return TestResponse.response(
                urlRequest, status: 200,
                json: peerAddressStatusJSON(peer: peer, address: currentAddress)
            )
        case 2:
            try expectPeerAddressRequest(urlRequest, equals: confirmed)
            return TestResponse.response(
                urlRequest, status: 200,
                json: #"{"offerId":null,"lifecycle":"running","issue":null}"#
            )
        case 3:
            return TestResponse.response(
                urlRequest, status: 200,
                json: peerAddressStatusJSON(peer: peer, address: confirmed.candidateAddress)
            )
        default:
            Issue.record("Unexpected address conflict request")
            return TestResponse.response(urlRequest, status: 500, json: "{}")
        }
    }
    let model = try peerAddressModel(recorder: recorder, fixture: fixture)
    let first = await model.refreshPeerAddress(stale)
    guard case let .requiresConfirmation(currentPeer, candidate, failure) = first else {
        Issue.record("A stale expected address must require new confirmation")
        return
    }
    #expect(currentPeer.address == currentAddress)
    #expect(candidate == stale.candidateAddress)
    #expect(failure.recovery == .none)
    #expect(sequence.count == 2)

    let second = await model.refreshPeerAddress(confirmed)
    guard case let .saved(savedPeer) = second else {
        Issue.record("Expected a separately confirmed current address to save")
        return
    }
    #expect(savedPeer.address == confirmed.candidateAddress)
    #expect(sequence.count == 4)
}

@Test @MainActor func acknowledgedProviderAddressClearsStaleReachabilityWhenReloadFails() async throws {
    let fixture = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
    defer { try? FileManager.default.removeItem(at: fixture) }
    let peer = UUID()
    let expected = "192.0.2.60:8787"
    let candidate = "192.0.2.61:8787"
    let fingerprint = String(repeating: "b", count: 64)
    let providerCalls = RequestSequence()
    let statusCalls = RequestSequence()
    let recorder = RequestRecorder(removeAfterRequest: false) { request in
        switch request.url?.path {
        case "/api/v1/status":
            return TestResponse.response(
                request, status: 200,
                json: #"{"deviceName":"Test Mac","protocolVersion":1,"lanDiscovery":false,"platformTier":"tier1","state":"ready"}"#
            )
        case "/api/v1/config/export":
            return TestResponse.response(
                request, status: 200,
                json: #"{"schemaVersion":1,"deviceName":"Test Mac","lanDiscoveryEnabled":false,"rememberedBackups":[]}"#
            )
        case "/api/v1/backups", "/api/v1/discovery":
            return TestResponse.response(request, status: 200, json: "[]")
        case "/api/v1/providers":
            if providerCalls.next() == 0 {
                return TestResponse.response(
                    request, status: 200,
                    json: "[{\"peerId\":\"\(peer.uuidString)\",\"address\":\"\(expected)\",\"certificateFingerprint\":\"\(fingerprint)\",\"reachability\":\"reachable\",\"observedAtUnixMs\":1,\"validUntilUnixMs\":4102444800000,\"usableBytes\":1,\"allocatedBytes\":1,\"quotaBytes\":2}]"
                )
            }
            return TestResponse.response(
                request, status: 503,
                json: #"{"protocolVersion":1,"code":"temporarily_unavailable","message":"retry","retryable":true}"#
            )
        case "/api/v1/sync/status":
            let address = statusCalls.next() == 0 ? expected : candidate
            return TestResponse.response(
                request, status: 200,
                json: peerAddressStatusJSON(peer: peer, address: address)
            )
        case "/api/v1/sync/peers/refresh-address":
            try expectPeerAddressRequest(
                request,
                equals: PeerAddressRefreshRequest(
                    peerId: peer,
                    expectedAddress: expected,
                    candidateAddress: candidate
                )
            )
            return TestResponse.response(
                request, status: 200,
                json: #"{"offerId":null,"lifecycle":"running","issue":null}"#
            )
        default:
            Issue.record("Unexpected provider address request: \(request.url?.path ?? "missing")")
            return TestResponse.response(request, status: 500, json: "{}")
        }
    }
    let model = try peerAddressModel(recorder: recorder, fixture: fixture)
    await model.start()
    #expect(model.providers.first?.reachability == .reachable)
    let outcome = await model.refreshPeerAddress(PeerAddressRefreshRequest(
        peerId: peer,
        expectedAddress: expected,
        candidateAddress: candidate
    ))
    guard case let .savedNeedsReload(savedPeer, savedAddress, failure) = outcome else {
        Issue.record("Expected a truthful saved-but-needs-reload result")
        return
    }
    #expect(savedPeer == peer)
    #expect(savedAddress == candidate)
    #expect(failure.recovery == .retry)
    #expect(model.folderSyncStatus?.peers.first?.address == candidate)
    #expect(model.providers.first?.address == candidate)
    #expect(model.providers.first?.reachability == nil)
    #expect(model.providers.first?.observedAtUnixMs == nil)
    #expect(model.providers.first?.validUntilUnixMs == nil)
}

@MainActor private func peerAddressModel(
    recorder: RequestRecorder,
    fixture: URL
) throws -> CovalentAppModel {
    let port = RecordingURLProtocol.recorder.install(recorder)
    let configuration = try NodeConnectionConfiguration(
        baseURL: URL(string: "http://127.0.0.1:\(port)")!,
        apiToken: String(repeating: "d", count: 32)
    )
    let sessionConfiguration = URLSessionConfiguration.ephemeral
    sessionConfiguration.protocolClasses = [RecordingURLProtocol.self]
    return CovalentAppModel(
        persistence: AppleAppPersistence(directoryURL: fixture.appending(path: "state")),
        client: NodeClient(
            configuration: configuration,
            session: URLSession(configuration: sessionConfiguration)
        ),
        configuration: configuration
    )
}

private func peerAddressStatusJSON(peer: UUID, address: String) -> String {
    "{\"schemaVersion\":1,\"availability\":\"available\",\"lifecycle\":\"running\",\"issue\":null,\"healthFreshness\":\"fresh\",\"connectionFreshness\":\"fresh\",\"peers\":[{\"peerId\":\"\(peer.uuidString)\",\"displayName\":\"Studio Mac\",\"address\":\"\(address)\"}],\"shares\":[],\"folders\":[]}"
}

private func peerAddressStatus(peer: FolderSyncPeer) -> FolderSyncStatus {
    FolderSyncStatus(
        availability: "available",
        lifecycle: "running",
        issue: nil,
        healthFreshness: "fresh",
        connectionFreshness: "fresh",
        peers: [peer],
        shares: [],
        folders: []
    )
}

private func expectPeerAddressRequest(
    _ request: URLRequest,
    equals expected: PeerAddressRefreshRequest
) throws {
    #expect(request.url?.path == "/api/v1/sync/peers/refresh-address")
    #expect(request.httpMethod == "POST")
    let body = try #require(folderRepairRequestBody(request))
    let object = try #require(JSONSerialization.jsonObject(with: body) as? [String: String])
    #expect(Set(object.keys) == Set(["peerId", "expectedAddress", "candidateAddress"]))
    #expect(UUID(uuidString: try #require(object["peerId"])) == expected.peerId)
    #expect(object["expectedAddress"] == expected.expectedAddress)
    #expect(object["candidateAddress"] == expected.candidateAddress)
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

@Test @MainActor func addingDestinationReusesLinkAndKeepsBothGrantBindingsUntilRemoval() async throws {
    let fixture = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
    defer { try? FileManager.default.removeItem(at: fixture) }
    let source = fixture.appending(path: "source")
    try FileManager.default.createDirectory(at: source, withIntermediateDirectories: true)
    let grant = try SelectedDirectoryGrant.capture(url: source, purpose: .folderSync)
    let folder = UUID()
    let firstPeer = UUID()
    let secondPeer = UUID()
    let firstOffer = UUID()
    let secondOffer = UUID()
    let policy = FolderLinkPolicy(propagateSourceDeletions: true, restoreLocalDeletions: false)
    let settingsState = FolderLinkSettingsState(
        revision: 1,
        settings: FolderLinkSettings(deletionPolicy: policy, paused: false),
        changeId: UUID(), changedBy: firstPeer, confirmed: true,
        pendingChange: nil, conflictedChange: nil
    )
    let peers = [
        FolderSyncPeer(peerId: firstPeer, displayName: "First Mac"),
        FolderSyncPeer(peerId: secondPeer, displayName: "Second Mac"),
    ]
    let firstShare = FolderShare(
        offerId: firstOffer, folderId: folder, label: "Photos", peerId: firstPeer,
        incoming: false, phase: .ready, expiresAtUnixMs: nil, expired: false,
        peerConnection: .connected, linkPolicy: policy, linkSettings: settingsState
    )
    let secondShare = FolderShare(
        offerId: secondOffer, folderId: folder, label: "Photos", peerId: secondPeer,
        incoming: false, phase: .offered, expiresAtUnixMs: 10, expired: false,
        peerConnection: .connected, linkPolicy: policy, linkSettings: settingsState
    )
    let firstStatus = try encodedFolderStatus(peers: peers, shares: [firstShare])
    let fanoutStatus = try encodedFolderStatus(peers: peers, shares: [firstShare, secondShare])
    let repairStatus = try encodedFolderStatus(
        peers: peers, shares: [firstShare, secondShare], issue: "folderAccess"
    )
    let retainedStatus = try encodedFolderStatus(peers: peers, shares: [secondShare])
    let sequence = RequestSequence()
    let offerBodies = FolderRepairRequestRoots()
    let recorder = RequestRecorder(removeAfterRequest: false) { request in
        switch sequence.next() {
        case 0:
            #expect(request.url?.path == "/api/v1/sync/folders")
            return TestResponse.response(request, status: 200, json: mutationJSON(offer: firstOffer))
        case 1:
            return TestResponse.response(request, status: 200, json: firstStatus)
        case 2:
            #expect(request.url?.path == "/api/v1/sync/folders")
            offerBodies.append(String(decoding: try #require(folderRepairRequestBody(request)), as: UTF8.self))
            return TestResponse.response(request, status: 200, json: mutationJSON(offer: secondOffer))
        case 3:
            return TestResponse.response(request, status: 200, json: fanoutStatus)
        case 4:
            return TestResponse.response(request, status: 200, json: repairStatus)
        case 5:
            #expect(request.url?.path == "/api/v1/sync/repair")
            return TestResponse.response(request, status: 200, json: mutationJSON(offer: firstOffer))
        case 6:
            return TestResponse.response(request, status: 200, json: fanoutStatus)
        case 7:
            #expect(request.url?.path == "/api/v1/sync/remove")
            return TestResponse.response(request, status: 200, json: mutationJSON(offer: firstOffer))
        case 8:
            return TestResponse.response(request, status: 200, json: retainedStatus)
        default:
            Issue.record("Unexpected fan-out request")
            return TestResponse.response(request, status: 500, json: "{}")
        }
    }
    let persistence = AppleAppPersistence(directoryURL: fixture.appending(path: "state"))
    let (model, bootstrapper) = try folderMutationModel(recorder: recorder, persistence: persistence)

    #expect(await model.offerFolder(
        peerId: firstPeer, folderId: folder, label: "Photos", grant: grant, linkPolicy: policy
    ))
    #expect(await model.addFolderDestination(from: firstOffer, to: secondPeer))

    let body = try #require(offerBodies.values.first)
    let payload = try #require(JSONSerialization.jsonObject(with: Data(body.utf8)) as? [String: Any])
    #expect(UUID(uuidString: try #require(payload["peerId"] as? String)) == secondPeer)
    #expect(UUID(uuidString: try #require(payload["folderId"] as? String)) == folder)
    #expect(payload["label"] as? String == "Photos")
    let selectedRoot = try #require(payload["selectedRoot"] as? String)
    #expect(selectedRoot == source.path || selectedRoot == "/private\(source.path)")
    #expect(payload["linkPolicy"] as? [String: Bool] == [
        "propagateSourceDeletions": true,
        "restoreLocalDeletions": false,
    ])
    #expect(model.directoryGrants.count == 2)
    #expect(Set(model.directoryGrants.compactMap(\.folderOfferId)) == [firstOffer, secondOffer])
    #expect(Set(model.directoryGrants.map(\.bookmarkData)) == [grant.bookmarkData])
    #expect(Set(model.directoryGrants.map(\.id)).count == 2)
    #expect(bootstrapper.restartCalls == 2)

    await model.refreshFolders()
    let replacement = try SelectedDirectoryGrant.capture(url: source, purpose: .folderSync)
    #expect(await model.repairFolderAccess(offerId: firstOffer, grant: replacement))
    #expect(model.directoryGrants.count == 2)
    #expect(Set(model.directoryGrants.compactMap(\.folderOfferId)) == [firstOffer, secondOffer])
    #expect(Set(model.directoryGrants.map(\.bookmarkData)) == [replacement.bookmarkData])
    #expect(bootstrapper.pendingRepairRestartCalls == 1)
    #expect(bootstrapper.lastPendingRepairGrants.count == 2)
    #expect(Set(bootstrapper.lastPendingRepairGrants.map(\.bookmarkData)) == [replacement.bookmarkData])

    await model.removeFolder(firstOffer)
    #expect(model.directoryGrants.count == 1)
    #expect(model.directoryGrants.first?.folderOfferId == secondOffer)
    #expect(model.directoryGrants.first?.bookmarkData == replacement.bookmarkData)
    let persisted = try await persistence.loadDirectoryGrants()
    #expect(persisted.map(\.id) == model.directoryGrants.map(\.id))
    #expect(persisted.map(\.folderOfferId) == model.directoryGrants.map(\.folderOfferId))
    #expect(persisted.map(\.bookmarkData) == model.directoryGrants.map(\.bookmarkData))
    #expect(bootstrapper.restartCalls == 4)
    #expect(sequence.count == 9)
}

private func encodedFolderStatus(
    peers: [FolderSyncPeer],
    shares: [FolderShare],
    issue: String? = nil
) throws -> String {
    let status = FolderSyncStatus(
        availability: "available", lifecycle: "running", issue: issue,
        healthFreshness: "fresh", connectionFreshness: "fresh",
        peers: peers, shares: shares, folders: []
    )
    return String(decoding: try JSONEncoder().encode(status), as: UTF8.self)
}

@Test @MainActor func uncertainLinkSettingsRetryUsesExactDurableRequestUntilStatusConfirmsIt() async throws {
    let fixture = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString)
    defer { try? FileManager.default.removeItem(at: fixture) }
    let folder = UUID()
    let peer = UUID()
    let offer = UUID()
    let policy = FolderLinkPolicy()
    let initialState = FolderLinkSettingsState(
        revision: 3,
        settings: FolderLinkSettings(deletionPolicy: policy, paused: false),
        changeId: UUID(), changedBy: peer, confirmed: true,
        pendingChange: nil, conflictedChange: nil
    )
    let initialShare = FolderShare(
        offerId: offer, folderId: folder, label: "Photos", peerId: peer,
        incoming: false, phase: .ready, expiresAtUnixMs: nil, expired: false,
        peerConnection: .connected, linkPolicy: policy, linkSettings: initialState
    )
    let initialStatus = try encodedFolderStatus(
        peers: [FolderSyncPeer(peerId: peer, displayName: "Mac")], shares: [initialShare]
    )
    let sequence = RequestSequence()
    let bodies = FolderRepairRequestRoots()
    let recorder = RequestRecorder(removeAfterRequest: false) { request in
        switch sequence.next() {
        case 0:
            return TestResponse.response(request, status: 200, json: initialStatus)
        case 1:
            #expect(request.url?.path == "/api/v1/sync/settings")
            bodies.append(String(decoding: try #require(folderRepairRequestBody(request)), as: UTF8.self))
            throw URLError(.networkConnectionLost)
        case 2:
            #expect(request.url?.path == "/api/v1/sync/settings")
            bodies.append(String(decoding: try #require(folderRepairRequestBody(request)), as: UTF8.self))
            return TestResponse.response(
                request, status: 200,
                json: #"{"offerId":null,"lifecycle":"running","issue":null}"#
            )
        case 3:
            let firstBody = try #require(bodies.values.first)
            let object = try #require(JSONSerialization.jsonObject(
                with: Data(firstBody.utf8)
            ) as? [String: Any])
            let changeIdText = try #require(object["changeId"] as? String)
            let changeId = try #require(UUID(uuidString: changeIdText))
            let updatedPolicy = FolderLinkPolicy(
                propagateSourceDeletions: true, restoreLocalDeletions: false
            )
            let appliedState = FolderLinkSettingsState(
                revision: 4,
                settings: FolderLinkSettings(deletionPolicy: updatedPolicy, paused: false),
                changeId: changeId, changedBy: peer, confirmed: true,
                pendingChange: nil, conflictedChange: nil
            )
            let appliedShare = FolderShare(
                offerId: offer, folderId: folder, label: "Photos", peerId: peer,
                incoming: false, phase: .ready, expiresAtUnixMs: nil, expired: false,
                peerConnection: .connected, linkPolicy: updatedPolicy, linkSettings: appliedState
            )
            return TestResponse.response(
                request, status: 200,
                json: try encodedFolderStatus(
                    peers: [FolderSyncPeer(peerId: peer, displayName: "Mac")],
                    shares: [appliedShare]
                )
            )
        default:
            Issue.record("Unexpected link settings request")
            return TestResponse.response(request, status: 500, json: "{}")
        }
    }
    let persistence = AppleAppPersistence(directoryURL: fixture.appending(path: "state"))
    let (model, _) = try folderMutationModel(recorder: recorder, persistence: persistence)
    await model.refreshFolders()

    let requested = FolderLinkSettings(
        deletionPolicy: FolderLinkPolicy(
            propagateSourceDeletions: true, restoreLocalDeletions: false
        ),
        paused: false
    )
    #expect(!(await model.updateFolderLinkSettings(
        folderId: folder, expectedRevision: 3, settings: requested
    )))
    #expect(model.pendingFolderLinkSettingsChanges.count == 1)
    #expect(try await persistence.loadPendingFolderLinkSettingsChanges()
        == model.pendingFolderLinkSettingsChanges)

    let retry = try #require(model.takeAlertRecovery())
    await retry()
    #expect(bodies.values.count == 2)
    #expect(bodies.values[0] == bodies.values[1])
    #expect(model.pendingFolderLinkSettingsChanges.isEmpty)
    #expect(try await persistence.loadPendingFolderLinkSettingsChanges().isEmpty)
    #expect(sequence.count == 4)
}

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
