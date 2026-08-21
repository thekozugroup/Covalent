import Foundation
import Testing

@testable import CovalentShared

/// Coverage for the transport path that every other test in this target skips.
///
/// `makeClient(recorder:token:)` installs a `URLProtocol` stub, so those tests
/// prove only that `NodeClient` *composes* the right request — they never hand
/// it to URLSession, never run CFNetwork, and never let CFNetwork consume the
/// upload's `httpBodyStream`. An `NSInputStream` class-cluster trap in exactly
/// that handoff crashed `integration-test.sh` while `swift test` stayed green.
///
/// These tests use a real `URLSession` (the one `NodeClient` builds for itself)
/// against a real loopback socket, so the whole stack is genuinely exercised.
@Suite struct RealTransportTests {
    @Test func statusTraversesRealCFNetworkRatherThanAURLProtocolStub() async throws {
        let server = try LoopbackHTTPServer { _ in
            .json(
                #"{"deviceName":"Loopback Node","protocolVersion":1,"lanDiscovery":true,"platformTier":"tier1","state":"ready"}"#
            )
        }
        let port = try await server.start()
        defer { server.stop() }

        // No `session:` argument: NodeClient builds its own real URLSession.
        let client = NodeClient(
            configuration: try NodeConnectionConfiguration(
                baseURL: try #require(URL(string: "http://127.0.0.1:\(port)")),
                apiToken: nil
            )
        )
        let status = try await client.status()
        #expect(status.deviceName == "Loopback Node")
        #expect(server.requests.first?.path == "/api/v1/status")
    }

    /// The regression this suite exists for: the archive upload streams its
    /// body from disk via `httpBodyStream`, and only CFNetwork actually reads
    /// that stream. This drives a real backup end to end and checks the bytes
    /// that arrived match the bytes on disk.
    @Test func archiveUploadStreamsItsBodyThroughRealCFNetwork() async throws {
        let root = FileManager.default.temporaryDirectory
            .appending(path: "covalent-real-transport-\(UUID().uuidString)", directoryHint: .isDirectory)
        let source = root.appending(path: "source", directoryHint: .isDirectory)
        let transfers = root.appending(path: "transfers", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: source, withIntermediateDirectories: true)
        try FileManager.default.createDirectory(at: transfers, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }

        // Incompressible, so the staged archive stays large and CFNetwork has
        // to pull the body stream in several reads rather than swallowing it
        // whole. A compressible pattern here would shrink the zip to a couple
        // of KiB and quietly stop exercising the chunked path.
        var generator = SystemRandomNumberGenerator()
        let payload = Data((0..<(512 * 1_024)).map { _ in UInt8.random(in: .min ... .max, using: &generator) })
        try payload.write(to: source.appending(path: "payload.bin", directoryHint: .notDirectory))
        try Data("hello".utf8).write(to: source.appending(path: "note.txt", directoryHint: .notDirectory))

        let backupId = UUID()
        let server = try LoopbackHTTPServer { request in
            guard request.path == "/api/v1/backups/archive" else {
                return .json(#"{"protocolVersion":1,"code":"route_not_found","message":"no","retryable":false}"#, status: 404)
            }
            return .json(
                """
                {"backupId":"\(backupId.uuidString.lowercased())","snapshotId":"s1","entries":2,\
                "bytesRead":\(payload.count),"chunksStored":2,"chunksDeduplicated":0,\
                "selectedProviders":0,"degradedFailures":0}
                """,
                headers: [AppleArchiveTransfer.jobAcknowledgementRequiredHeader: "true"]
            )
        }
        let port = try await server.start()
        defer { server.stop() }

        let client = NodeClient(
            configuration: try NodeConnectionConfiguration(
                baseURL: try #require(URL(string: "http://127.0.0.1:\(port)")),
                apiToken: String(repeating: "a", count: 32)
            ),
            transferDirectory: transfers
        )

        let progress = ProgressLog()
        let response = try await client.createBackupArchive(
            sourceURL: source,
            metadata: ArchiveBackupMetadata(
                displayName: "Loopback backup",
                snapshotId: "s1",
                jobId: "backup-real-transport",
                selectedProviderIds: []
            ),
            onProgress: { progress.record($0) }
        )
        #expect(response.backupId == backupId)

        let uploaded = try #require(server.requests.first)
        #expect(uploaded.method == "POST")
        #expect(uploaded.header("Content-Type") == AppleArchiveTransfer.backupContentType)
        #expect(uploaded.header(AppleArchiveTransfer.uploadOffsetHeader) == "0")

        // CFNetwork consumed the whole file-backed stream: the declared length,
        // the framed Content-Length and the bytes actually received all agree.
        let declaredLength = try #require(uploaded.header(AppleArchiveTransfer.uploadLengthHeader).flatMap(Int.init))
        #expect(declaredLength > 0)
        #expect(uploaded.body.count == declaredLength)
        #expect(uploaded.header("Content-Length").flatMap(Int.init) == declaredLength)

        // And the progress the UI renders came from those same real bytes.
        let snapshots = progress.snapshots
        #expect(snapshots.first?.phase == .preparing)
        #expect(snapshots.last?.phase == .finishing)
        #expect(snapshots.contains { $0.phase == .transferring && $0.fractionCompleted != nil })
        #expect(snapshots.last?.completedBytes == UInt64(declaredLength))
    }

    /// A structured engine error must survive the real stack and arrive as
    /// plain-English copy with its technical detail kept separate.
    @Test func engineErrorCodesArriveAsHumanCopyOverRealCFNetwork() async throws {
        let server = try LoopbackHTTPServer { _ in
            .json(
                #"{"protocolVersion":1,"code":"insufficient_storage","message":"The node does not have enough reserved capacity for this archive.","retryable":false}"#,
                status: 507
            )
        }
        let port = try await server.start()
        defer { server.stop() }

        let client = NodeClient(
            configuration: try NodeConnectionConfiguration(
                baseURL: try #require(URL(string: "http://127.0.0.1:\(port)")),
                apiToken: String(repeating: "b", count: 32)
            )
        )
        await #expect(throws: NodeClientError.self) {
            _ = try await client.exportSettings()
        }
        do {
            _ = try await client.exportSettings()
        } catch let error as NodeClientError {
            let described = try #require(error.errorDescription)
            #expect(described.contains("out of space"))
            #expect(!described.contains("reserved capacity"), "engine wording must not lead the message")
            #expect(error.diagnosticDetail?.contains("insufficient_storage") == true)
            #expect(error.recoveryHint == .freeUpSpace)
        }
    }

    /// A refused connection must not surface `NSURLErrorDomain Code=-1004`.
    @Test func refusedConnectionsBecomeCopyAPersonCanAct() async throws {
        // Bind and immediately release a port so nothing is listening on it.
        let idle = try LoopbackHTTPServer { _ in .json("{}") }
        let port = try await idle.start()
        idle.stop()
        try await Task.sleep(for: .milliseconds(150))

        let client = NodeClient(
            configuration: try NodeConnectionConfiguration(
                baseURL: try #require(URL(string: "http://127.0.0.1:\(port)")),
                apiToken: nil
            )
        )
        do {
            _ = try await client.status()
            Issue.record("Expected the unreachable node to fail.")
        } catch let error as NodeClientError {
            let described = try #require(error.errorDescription)
            #expect(!described.contains("NSURLErrorDomain"))
            #expect(!described.contains("Error Domain"))
            #expect(!described.contains("UserInfo"))
            #expect(described.contains("backup server"))
            #expect(error.recoveryHint != RecoveryHint.none)
            // The raw text is still available, just not leading.
            #expect(error.diagnosticDetail?.isEmpty == false)
        }
    }
}

/// Collects progress callbacks, which arrive on URLSession's queue.
private final class ProgressLog: @unchecked Sendable {
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
