import Foundation
import Testing

@testable import CovalentShared

/// The version, release check and failure causes the service added after
/// 0.2.12. Every one of them is optional: the app must look and behave exactly
/// as before when talking to an older service, and must never let one of
/// these extras cost the person their connection or raise an alert.
@Suite struct ReleaseAndRunFailureTests {
    private static let releasePage = "https://github.com/thekozugroup/Covalent/releases/tag/v0.2.13"

    private static func status(_ extra: String = "") throws -> NodeStatus {
        let json = #"{"deviceName":"Atlas","protocolVersion":1,"lanDiscovery":false,"platformTier":"tier1","state":"ready"\#(extra)}"#
        return try JSONDecoder().decode(NodeStatus.self, from: Data(json.utf8))
    }

    private static func update(_ state: String, latest: String? = nil, url: String? = nil, failure: String = "null") -> String {
        let latestJSON = latest.map { "\"\($0)\"" } ?? "null"
        let urlJSON = url.map { "\"\($0)\"" } ?? "null"
        return #","version":"0.2.12","update":{"state":"\#(state)","latestVersion":\#(latestJSON),"releaseUrl":\#(urlJSON),"checkedAtUnixMs":1794360000000,"failure":\#(failure)}"#
    }

    // MARK: Status from an older service

    @Test func statusFromAnOlderServiceShowsNothingNew() throws {
        let status = try Self.status()
        #expect(status.version == nil)
        #expect(status.update == nil)
        #expect(status.displayVersion == nil)
        #expect(status.availableUpdate == nil)
        #expect(status.updateCheckSummary == nil)
    }

    @Test func malformedExtrasReadAsAbsentInsteadOfFailingTheStatus() throws {
        let wrongTypes = try Self.status(#","version":12,"update":"soon""#)
        #expect(wrongTypes.deviceName == "Atlas")
        #expect(wrongTypes.version == nil)
        #expect(wrongTypes.update == nil)

        let newerState = try Self.status(Self.update("somethingNew", latest: "0.2.13", url: Self.releasePage))
        #expect(newerState.update?.knownState == nil)
        #expect(newerState.availableUpdate == nil)
        #expect(newerState.updateCheckSummary == nil)
    }

    @Test func versionIsShownOnlyWhenItIsAPlainVersion() throws {
        #expect(try Self.status(#","version":"0.2.12""#).displayVersion == "0.2.12")
        #expect(try Self.status(#","version":"0.3.0-rc.1""#).displayVersion == "0.3.0-rc.1")
        #expect(try Self.status(#","version":"""#).displayVersion == nil)
        #expect(try Self.status(#","version":"0.2.12 <b>""#).displayVersion == nil)
        #expect(try Self.status(#","version":"\#(String(repeating: "9", count: 40))""#).displayVersion == nil)
    }

    // MARK: Update available

    @Test func anAvailableUpdateOffersTheProjectsOwnReleasePage() throws {
        let status = try Self.status(Self.update("updateAvailable", latest: "0.2.13", url: Self.releasePage))
        let update = try #require(status.availableUpdate)
        #expect(update.version == "0.2.13")
        #expect(update.releaseURL.absoluteString == Self.releasePage)
        // Shown as its own notice, never also as a summary line.
        #expect(status.updateCheckSummary == nil)
    }

    @Test(arguments: [
        "http://github.com/thekozugroup/Covalent/releases/tag/v0.2.13",
        "https://github.com/someone-else/Covalent/releases/tag/v0.2.13",
        "https://github.com.example.net/thekozugroup/Covalent/releases/tag/v0.2.13",
        "https://example.net/?https://github.com/thekozugroup/Covalent/releases/tag/v0.2.13",
        "javascript:alert(1)",
        // The right prefix is not enough: it must be exactly the reported version's page.
        "https://github.com/thekozugroup/Covalent/releases/tag/v0.2.13/../../../../evil",
        "https://github.com/thekozugroup/Covalent/releases/tag/v0.2.13/extra",
        "https://github.com/thekozugroup/Covalent/releases/tag/v0.2.13?next=https://example.net",
        "https://github.com/thekozugroup/Covalent/releases/tag/v0.2.13#fragment",
        "https://github.com/thekozugroup/Covalent/releases/tag/v0.2.14",
        "https://github.com/thekozugroup/Covalent/releases/tag/v0.2.1",
    ])
    func anyOtherLinkIsNeverOffered(link: String) throws {
        let status = try Self.status(Self.update("updateAvailable", latest: "0.2.13", url: link))
        #expect(status.availableUpdate == nil)
    }

    @Test func anUpdateWithoutAVersionOrLinkIsNotOffered() throws {
        #expect(try Self.status(Self.update("updateAvailable", url: Self.releasePage)).availableUpdate == nil)
        #expect(try Self.status(Self.update("updateAvailable", latest: "0.2.13")).availableUpdate == nil)
        #expect(try Self.status(Self.update("upToDate", latest: "0.2.13", url: Self.releasePage)).availableUpdate == nil)
    }

    // MARK: Quiet lines for every other state

    @Test func everyOtherStateIsQuietAndNeverAnError() throws {
        #expect(try Self.status(Self.update("upToDate", latest: "0.2.12")).updateCheckSummary == "Covalent is up to date.")
        #expect(try Self.status(Self.update("disabled")).updateCheckSummary == "Update checks are off.")
        #expect(try Self.status(Self.update("pending")).updateCheckSummary == nil)
        #expect(try Self.status(Self.update("unsupported")).updateCheckSummary == nil)
    }

    @Test(arguments: ["network", "timeout", "secureConnection", "rateLimited", "unexpectedResponse", "aCodeAddedLater"])
    func aFailedCheckIsExplainedInPlainWords(code: String) throws {
        let failure = #"{"code":"\#(code)","message":"Raw service sentence."}"#
        let summary = try #require(try Self.status(Self.update("checkFailed", failure: failure)).updateCheckSummary)
        #expect(summary.hasPrefix("Couldn't check for updates. "))
        #expect(!summary.contains("Raw service sentence."))
        #expect(!summary.localizedCaseInsensitiveContains("error"))
    }

    @Test func anUnknownCheckFailureReadsLikeAnUnexpectedAnswer() {
        #expect(NodeStatus.updateCheckReason(code: "aCodeAddedLater") == NodeStatus.updateCheckReason(code: "unexpectedResponse"))
        #expect(NodeStatus.updateCheckReason(code: nil) == NodeStatus.updateCheckReason(code: "unexpectedResponse"))
    }

    // MARK: Failure causes

    private static let folder = UUID(uuidString: "0B6F0E3A-5C0E-4F4E-9A51-3F1D2A7C9E10")!

    private static func share(folder: UUID = folder, generation: UInt64) -> FolderShare {
        FolderShare(
            offerId: UUID(), folderId: folder, label: "Photos", peerId: UUID(),
            incoming: true, phase: .ready, expiresAtUnixMs: nil, expired: false,
            peerConnection: .connected, linkPolicy: FolderLinkPolicy(), linkSettings: nil,
            linkRun: FolderLinkRunSummary(
                generation: generation, stateRevision: 1, settingsRevision: 1, phase: .incomplete,
                startedAtUnixMs: 1, deadlineUnixMs: 2, endedAtUnixMs: 2, nextDueAtUnixMs: nil,
                pendingRequest: nil, rejectedRequest: nil, destinations: []
            )
        )
    }

    private static func failure(code: String, role: String = "destination", generation: UInt64 = 42, message: String = "x") -> FolderRunFailure {
        FolderRunFailure(
            folderId: folder, generation: generation, role: role,
            code: code, message: message, endedAtUnixMs: 1_794_360_000_000
        )
    }

    @Test func aCauseIsShownOnlyForTheRunOnScreenAndTheRightRole() {
        let failures = FolderRunFailures(failures: [Self.failure(code: "noSpace")])
        #expect(failures.failure(for: Self.share(generation: 42), role: .destination)?.code == "noSpace")
        // A newer run has started: the entry is about the previous one.
        #expect(failures.failure(for: Self.share(generation: 43), role: .destination) == nil)
        #expect(failures.failure(for: Self.share(folder: UUID(), generation: 42), role: .destination) == nil)
        #expect(failures.failure(for: Self.share(generation: 42), role: .source) == nil)
    }

    @Test func everyKnownCauseHasItsOwnPlainCopyInGlossaryWords() {
        let codes = [
            "noSpace", "permissionDenied", "readOnly", "integrityMismatch", "notFound", "network",
            "timeout", "sourceChanged", "folderUnavailable", "recoveryRequired", "workerUnavailable", "unknown",
        ]
        let copy = codes.map { Self.failure(code: $0, message: "SERVICE SENTENCE").plainCause }
        #expect(Set(copy).count == codes.count)
        for line in copy {
            #expect(!line.contains("SERVICE SENTENCE"))
            #expect(line.range(of: #"\bshares?\b"#, options: [.regularExpression, .caseInsensitive]) == nil, "\(line)")
            #expect(line.hasSuffix("."))
        }
        #expect(Self.failure(code: "sourceChanged").isInformational)
        #expect(!Self.failure(code: "noSpace").isInformational)
    }

    @Test func anUnrecognizedCauseFallsBackToTheServiceSentenceThenToAGenericLine() {
        #expect(Self.failure(code: "aCodeAddedLater", message: "The disk was unplugged.").plainCause == "The disk was unplugged.")
        #expect(Self.failure(code: "aCodeAddedLater", message: "  ").plainCause == FolderRunFailure.genericCause)
        #expect(Self.failure(code: "aCodeAddedLater", message: "two\nlines").plainCause == FolderRunFailure.genericCause)
        #expect(Self.failure(code: "unknown", message: "Anything.").plainCause == FolderRunFailure.genericCause)
    }
}

// MARK: - Client and model behaviour against a scripted service

private final class PathLog: @unchecked Sendable {
    private let lock = NSLock()
    private var paths: [String] = []
    private var statusBody: String?
    private var runFailuresStatus = 404
    private var runFailuresBody = "{}"

    var requested: [String] { lock.withLock { paths } }
    func record(_ path: String) { lock.withLock { paths.append(path) } }

    var status: String? {
        get { lock.withLock { statusBody } }
        set { lock.withLock { statusBody = newValue } }
    }

    var runFailures: (Int, String) {
        get { lock.withLock { (runFailuresStatus, runFailuresBody) } }
        set { lock.withLock { (runFailuresStatus, runFailuresBody) = newValue } }
    }
}

private let thisMacFolder = UUID(uuidString: "0B6F0E3A-5C0E-4F4E-9A51-3F1D2A7C9E10")!
private let sourcePeer = UUID(uuidString: "11111111-1111-4111-8111-111111111111")!
private let thisMacPeer = UUID(uuidString: "22222222-2222-4222-8222-222222222222")!

private func syncStatusJSON(failed: Bool) -> String {
    let destinations = failed
        ? #"[{"peerId":"\#(thisMacPeer.uuidString)","result":"failed","endedAtUnixMs":2000}]"#
        : #"[{"peerId":"\#(thisMacPeer.uuidString)","result":"succeeded","endedAtUnixMs":2000}]"#
    let phase = failed ? "incomplete" : "succeeded"
    return #"{"schemaVersion":1,"availability":"available","lifecycle":"running","issue":null,"healthFreshness":"fresh","connectionFreshness":"fresh","peers":[{"peerId":"\#(sourcePeer.uuidString)","displayName":"Studio Mac"}],"shares":[{"offerId":"33333333-3333-4333-8333-333333333333","folderId":"\#(thisMacFolder.uuidString)","label":"Photos","peerId":"\#(sourcePeer.uuidString)","incoming":true,"phase":"ready","expiresAtUnixMs":null,"expired":false,"peerConnection":"connected","linkPolicy":{"propagateSourceDeletions":false,"restoreLocalDeletions":false},"linkRun":{"generation":42,"stateRevision":5,"settingsRevision":4,"phase":"\#(phase)","startedAtUnixMs":1000,"deadlineUnixMs":3000,"endedAtUnixMs":2000,"nextDueAtUnixMs":null,"pendingRequest":null,"rejectedRequest":null,"destinations":\#(destinations)}}],"folders":[]}"#
}

private let olderServiceStatus = #"{"deviceName":"Atlas","protocolVersion":1,"lanDiscovery":false,"platformTier":"tier1","state":"ready"}"#
private let updateAvailableStatus = #"{"deviceName":"Atlas","protocolVersion":1,"lanDiscovery":false,"platformTier":"tier1","state":"ready","version":"0.2.12","update":{"state":"updateAvailable","latestVersion":"0.2.13","releaseUrl":"https://github.com/thekozugroup/Covalent/releases/tag/v0.2.13","checkedAtUnixMs":1794360000000,"failure":null}}"#

@MainActor
private func scriptedModel(log: PathLog, failedRun: Bool) throws -> (CovalentAppModel, URL) {
    let recorder = RequestRecorder(removeAfterRequest: false) { request in
        let path = request.url?.path ?? ""
        log.record(path)
        switch path {
        case "/api/v1/status":
            guard let body = log.status else {
                return TestResponse.response(request, status: 503, json: #"{"error":{"code":"busy","message":"busy"}}"#)
            }
            return TestResponse.response(request, status: 200, json: body)
        case "/api/v1/config/export":
            return TestResponse.response(
                request, status: 200,
                json: #"{"schemaVersion":1,"deviceName":"Atlas","lanDiscoveryEnabled":false,"rememberedBackups":[]}"#
            )
        case "/api/v1/providers", "/api/v1/backups", "/api/v1/discovery":
            return TestResponse.response(request, status: 200, json: "[]")
        case "/api/v1/sync/status":
            return TestResponse.response(request, status: 200, json: syncStatusJSON(failed: failedRun))
        case "/api/v1/sync/run-failures":
            let (status, body) = log.runFailures
            return TestResponse.response(request, status: status, json: body)
        default:
            return TestResponse.response(request, status: 404, json: #"{"error":{"code":"not_found","message":"Not found"}}"#)
        }
    }
    let port = RecordingURLProtocol.recorder.install(recorder)
    let configuration = try NodeConnectionConfiguration(
        baseURL: URL(string: "http://127.0.0.1:\(port)")!,
        apiToken: String(repeating: "r", count: 32)
    )
    let session = URLSessionConfiguration.ephemeral
    session.protocolClasses = [RecordingURLProtocol.self]
    let fixture = FileManager.default.temporaryDirectory.appending(path: UUID().uuidString, directoryHint: .isDirectory)
    let model = CovalentAppModel(
        persistence: AppleAppPersistence(directoryURL: fixture.appending(path: "state", directoryHint: .isDirectory)),
        client: NodeClient(configuration: configuration, session: URLSession(configuration: session)),
        configuration: configuration
    )
    return (model, fixture)
}

@Test @MainActor func aHealthyLinkListNeverAsksForFailureCauses() async throws {
    let log = PathLog()
    let (model, fixture) = try scriptedModel(log: log, failedRun: false)
    defer { try? FileManager.default.removeItem(at: fixture) }
    await model.refreshFolders()
    #expect(model.folderSyncStatus != nil)
    #expect(!log.requested.contains("/api/v1/sync/run-failures"))
    #expect(model.folderRunFailures == nil)
}

@Test @MainActor func anOlderServiceWithoutCausesLeavesTodaysFailedRowAndRaisesNothing() async throws {
    let log = PathLog()
    log.runFailures = (404, #"{"error":{"code":"not_found","message":"Not found"}}"#)
    let (model, fixture) = try scriptedModel(log: log, failedRun: true)
    defer { try? FileManager.default.removeItem(at: fixture) }
    await model.refreshFolders()
    #expect(log.requested.contains("/api/v1/sync/run-failures"))
    #expect(model.folderSyncStatus?.shares.first?.linkRun?.destinations.first?.result == .failed)
    #expect(model.folderRunFailures == nil)
    #expect(model.folderSyncError == nil)
    #expect(model.alert == nil)
}

@Test @MainActor func aRefusedOrBrokenCauseRequestIsAlsoSilent() async throws {
    for (status, body) in [(401, #"{"error":{"code":"unauthorized","message":"No"}}"#), (200, "not json"), (200, #"{"schemaVersion":2,"failures":[]}"#)] {
        let log = PathLog()
        log.runFailures = (status, body)
        let (model, fixture) = try scriptedModel(log: log, failedRun: true)
        await model.refreshFolders()
        #expect(model.folderSyncStatus != nil)
        #expect(model.folderRunFailures == nil)
        #expect(model.folderSyncError == nil)
        #expect(model.alert == nil)
        try? FileManager.default.removeItem(at: fixture)
    }
}

@Test @MainActor func thisMacsOwnFailedCopyGetsItsCause() async throws {
    let log = PathLog()
    log.runFailures = (200, #"{"schemaVersion":1,"failures":[{"folderId":"\#(thisMacFolder.uuidString)","generation":42,"role":"destination","code":"noSpace","message":"This device ran out of storage space for the copy. Free up space, then run the share again.","endedAtUnixMs":1794360000000}]}"#)
    let (model, fixture) = try scriptedModel(log: log, failedRun: true)
    defer { try? FileManager.default.removeItem(at: fixture) }
    await model.refreshFolders()
    let share = try #require(model.folderSyncStatus?.shares.first)
    let failure = try #require(model.folderRunFailures?.failure(for: share, role: .destination))
    #expect(failure.plainCause == "This device ran out of storage space. Free up space, then try again.")
}

@Test @MainActor func theReleaseCheckIsReadQuietlyAndAFailedReReadKeepsTheLastAnswer() async throws {
    let log = PathLog()
    log.status = olderServiceStatus
    let (model, fixture) = try scriptedModel(log: log, failedRun: false)
    defer { try? FileManager.default.removeItem(at: fixture) }

    await model.refresh()
    #expect(model.phase == .ready)
    #expect(model.status?.availableUpdate == nil)

    log.status = updateAvailableStatus
    await model.refreshServiceStatus()
    #expect(model.status?.displayVersion == "0.2.12")
    #expect(model.status?.availableUpdate?.version == "0.2.13")

    log.status = nil
    await model.refreshServiceStatus()
    #expect(model.status?.availableUpdate?.version == "0.2.13")
    #expect(model.phase == .ready)
    #expect(model.alert == nil)
}

@Test func theClientTreatsAMissingRouteAsNoCauses() async throws {
    let recorder = RequestRecorder { request in
        TestResponse.response(request, status: 404, json: #"{"error":{"code":"not_found","message":"Not found"}}"#)
    }
    let port = RecordingURLProtocol.recorder.install(recorder)
    let session = URLSessionConfiguration.ephemeral
    session.protocolClasses = [RecordingURLProtocol.self]
    let client = NodeClient(
        configuration: try NodeConnectionConfiguration(
            baseURL: URL(string: "http://127.0.0.1:\(port)")!,
            apiToken: String(repeating: "c", count: 32)
        ),
        session: URLSession(configuration: session)
    )
    #expect(try await client.folderRunFailures() == nil)
}
