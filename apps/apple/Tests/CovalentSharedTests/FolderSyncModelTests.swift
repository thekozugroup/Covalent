import Foundation
import Testing
@testable import CovalentShared

@MainActor
private final class FolderGrantBootstrapper: LocalNodeBootstrapping {
    let configuration: NodeConnectionConfiguration
    var restartFailuresRemaining = 0
    private(set) var restartCalls = 0

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
}

private enum FolderGrantTestError: Error { case restartFailed }

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
    let grant = try SelectedDirectoryGrant.capture(url: source, purpose: .folderSync)

    let firstOffer = await model.offerFolder(peerId: peer, folderId: folder, label: "Plans", grant: grant)
    #expect(!firstOffer)
    let persistedGrants = try await persistence.loadDirectoryGrants()
    #expect(persistedGrants == [grant])

    let retriedOffer = await model.offerFolder(peerId: peer, folderId: folder, label: "Plans", grant: grant)
    #expect(retriedOffer)
    #expect(bootstrapper.restartCalls == 2)
}
