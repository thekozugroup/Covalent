#if os(macOS)
import Darwin
import Foundation
import Testing
@testable import CovalentShared

@Test func missingManagedNodeKeyIsProvisionedOnceAndThenReused() throws {
    let persistence = TestManagedNodeKeyPersistence()
    let store = ManagedNodeKeyStore(persistence: persistence) { count in
        [UInt8](repeating: 0x41, count: count)
    }

    let first = try store.loadOrCreate()
    let second = try store.loadOrCreate()

    #expect(first.currentVersion == 1)
    #expect(first.keyCount == 1)
    #expect(first.bytes == second.bytes)
    #expect(persistence.writeCount == 1)
}

@Test func corruptManagedNodeKeyHistoryFailsClosedWithoutReplacement() throws {
    let persistence = TestManagedNodeKeyPersistence(data: Data("not-a-key-hierarchy".utf8))
    let store = ManagedNodeKeyStore(persistence: persistence) { count in
        [UInt8](repeating: 0x42, count: count)
    }

    #expect(throws: ManagedNodeKeyStoreError.corruptHierarchy) {
        _ = try store.loadOrCreate()
    }
    #expect(persistence.writeCount == 0)
}

@Test func lockedKeychainHasActionableErrorAndDoesNotGenerateAReplacement() throws {
    let persistence = TestManagedNodeKeyPersistence(error: .keychainLocked)
    let store = ManagedNodeKeyStore(persistence: persistence) { _ in
        Issue.record("A locked Keychain must not generate a replacement key")
        return []
    }

    #expect(throws: ManagedNodeKeyStoreError.keychainLocked) {
        _ = try store.loadOrCreate()
    }
    #expect(ManagedNodeKeyStoreError.keychainLocked.localizedDescription.contains("Unlock this Mac"))
}

@Test func rotationAdvancesCurrentVersionAndRetainsHistory() throws {
    let persistence = TestManagedNodeKeyPersistence()
    let counter = LockedByteCounter()
    let store = ManagedNodeKeyStore(persistence: persistence) { count in
        [UInt8](repeating: counter.next(), count: count)
    }
    let first = try store.loadOrCreate()

    let rotated = try store.rotate()
    let entries = rotated.entries()

    #expect(rotated.currentVersion == 2)
    #expect(entries.map(\.version) == [1, 2])
    #expect(entries[0].bytes == [UInt8](repeating: 1, count: 32))
    #expect(entries[1].bytes == [UInt8](repeating: 3, count: 32))
    #expect(rotated.token == first.token, "KEK rotation must not replace the local API token")
}

@Test func rotatingAMissingHierarchyFailsInsteadOfSilentlyReplacingIt() throws {
    let persistence = TestManagedNodeKeyPersistence()
    let store = ManagedNodeKeyStore(persistence: persistence) { count in
        [UInt8](repeating: 0x43, count: count)
    }

    #expect(throws: ManagedNodeKeyStoreError.missingHierarchy) {
        _ = try store.rotate()
    }
    #expect(throws: ManagedNodeKeyStoreError.missingHierarchy) {
        _ = try store.loadExisting()
    }
}

@Test func concurrentFirstRunAdoptsTheAtomicKeychainWinner() throws {
    let winner = try ManagedNodeKeyMaterial(
        currentVersion: 1,
        keys: [(1, [UInt8](repeating: 0x52, count: 32))],
        apiToken: testTokenBytes
    )
    let persistence = RacingManagedNodeKeyPersistence(winner: Data(winner.bytes))
    let store = ManagedNodeKeyStore(persistence: persistence) { count in
        [UInt8](repeating: 0x41, count: count)
    }

    let adopted = try store.loadOrCreate()

    #expect(adopted.entries()[0].bytes == [UInt8](repeating: 0x52, count: 32))
}

@Test func inheritedPipeHandoffIsExactAndErasesTheSourceBuffer() throws {
    var descriptors = [Int32](repeating: -1, count: 2)
    #expect(Darwin.pipe(&descriptors) == 0)
    defer {
        Darwin.close(descriptors[0])
        Darwin.close(descriptors[1])
    }
    var material = try ManagedNodeKeyMaterial(
        currentVersion: 7,
        keys: [(7, [UInt8](repeating: 0x77, count: 32))],
        apiToken: testTokenBytes
    )
    let expectedCount = material.bytes.count

    try material.writeAndErase(to: descriptors[1])
    #expect(material.bytes.allSatisfy { $0 == 0 })

    var received = [UInt8](repeating: 0, count: expectedCount)
    let count = received.withUnsafeMutableBytes {
        Darwin.read(descriptors[0], $0.baseAddress, $0.count)
    }
    #expect(count == expectedCount)
    #expect(Array(received.prefix(8)) == Array("CVSEC002".utf8))
}

@Test func childExitBeforePipeHandoffCannotTerminateTheAppProcess() throws {
    let keyPipe = Pipe()
    let child = Process()
    child.executableURL = URL(fileURLWithPath: "/usr/bin/true")
    child.standardInput = keyPipe
    try child.run()
    child.waitUntilExit()
    #expect(child.terminationStatus == 0)
    try keyPipe.fileHandleForReading.close()
    defer { try? keyPipe.fileHandleForWriting.close() }

    var material = try ManagedNodeKeyMaterial(
        currentVersion: 9,
        keys: [(9, [UInt8](repeating: 0x79, count: 32))],
        apiToken: testTokenBytes
    )
    do {
        try material.writeAndErase(to: keyPipe.fileHandleForWriting.fileDescriptor)
        Issue.record("A closed helper pipe must report failure")
    } catch let error as ManagedNodeKeyStoreError {
        #expect(error == .pipeWriteFailed)
    }

    #expect(
        Darwin.fcntl(keyPipe.fileHandleForWriting.fileDescriptor, F_GETNOSIGPIPE) == 1,
        "The helper pipe must suppress SIGPIPE on its own descriptor"
    )
    #expect(material.bytes.allSatisfy { $0 == 0 })
}

@Test func managedNodeLifecycleAdoptsBeforeKeychainAndUsesNoSecretLaunchMetadata() throws {
    let appleRoot = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent()
        .deletingLastPathComponent()
        .deletingLastPathComponent()
    let manager = try String(
        contentsOf: appleRoot.appending(path: "Sources/CovalentMac/LocalNodeManager.swift"),
        encoding: .utf8
    )
    let reconnect = try #require(manager.range(of: "reconnectToExistingConfiguration(paths: paths)"))
    let keyLoad = try #require(manager.range(of: "keyStore.loadOrCreate()"))

    #expect(reconnect.lowerBound < keyLoad.lowerBound, "Crash adoption must not depend on a fresh Keychain read")
    #expect(manager.contains("process.standardInput = keyPipe"))
    #expect(manager.contains("\"--key-encryption-key-stdin\""))
    #expect(manager.contains("keyMaterial.writeAndErase"))
    #expect(manager.contains("hasProtectedLocalSecret"))
    #expect(manager.contains("keyStore.loadExisting()"))
    #expect(!manager.contains("local-api-token"))
    #expect(!manager.contains("process.environment"))
    #expect(!manager.contains("COVALENT_KEY_ENCRYPTION_KEY="))
}

@Test func appleHarnessesNeverExtractADaemonTokenOrPassItAsLaunchMetadata() throws {
    let appleRoot = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent()
        .deletingLastPathComponent()
        .deletingLastPathComponent()
    for script in ["Scripts/integration-test.sh", "Scripts/macos-ui-test.sh", "Scripts/ios-ui-test.sh"] {
        let source = try String(contentsOf: appleRoot.appending(path: script), encoding: .utf8)
        #expect(!source.contains("local-api-token"), "\(script) must not read a daemon token file")
        #expect(!source.contains("token=$(tr -d"), "\(script) must not extract a plaintext token")
        #expect(source.contains("--api-token-file"), "\(script) must use the owner-only harness input")
    }
}

@Test func appleUITestHarnessUsesOnlyAPrivateTokenPathAndReleaseBuildsIgnoreIt() throws {
    let appleRoot = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent()
        .deletingLastPathComponent()
        .deletingLastPathComponent()
    for script in ["Scripts/macos-ui-test.sh", "Scripts/ios-ui-test.sh"] {
        let source = try String(contentsOf: appleRoot.appending(path: script), encoding: .utf8)
        #expect(source.contains("COVALENT_UI_TEST_TOKEN_FILE"), "\(script) must pass only a token-file path")
        #expect(source.contains("copy-owner-only-token.py"), "\(script) must provision the target-app private file")
        #expect(source.contains("ui-token-$token_nonce"), "\(script) must use a unique relative filename")
        #expect(source.contains("rm -f -- \"$app_token_file\""), "\(script) must remove only its own token file")
        #expect(!source.contains("COVALENT_UI_TEST_TOKEN ="), "\(script) must not write raw token build settings")
    }

    let project = try String(contentsOf: appleRoot.appending(path: "Project.yml"), encoding: .utf8)
    let tokenFileSetting = "CovalentUITestTokenFile: $(COVALENT_UI_TEST_TOKEN_FILE)"
    #expect(project.components(separatedBy: tokenFileSetting).count == 3,
            "Both UI-test targets must receive only the private token-file path")
    #expect(!project.contains("CovalentUITestToken: $(COVALENT_UI_TEST_TOKEN)"))
    for plist in ["Config/CovalentMacUITests-Info.plist", "Config/CovalentIOSUITests-Info.plist"] {
        #expect(project.contains("path: \(plist)"), "Project.yml must generate \(plist)")
    }

    let model = try String(
        contentsOf: appleRoot.appending(path: "Sources/CovalentShared/CovalentAppModel.swift"),
        encoding: .utf8
    )
    #expect(model.contains("#if DEBUG\n            let environment = ProcessInfo.processInfo.environment"))
    #expect(model.contains("O_NOFOLLOW"), "UI-test token reads must reject symlinks")
    #expect(model.contains("metadata.st_uid == getuid()"))
    #expect(model.contains("mode_t(0o600)"))
    #expect(model.contains("#else\n            loadedConfiguration = (try? connectionStore.load()) ?? .localDefault"),
            "release builds must ignore the UI-test launch hook")
    #expect(!model.contains("environment[\"COVALENT_UI_TEST_TOKEN\"]"))
}

@Test func legacyKEKOnlyRecordMigratesAtomicallyToV2WithoutRotatingKeys() throws {
    let legacy = legacyBytes(version: 4, key: 0x61)
    let persistence = TestManagedNodeKeyPersistence(data: Data(legacy))
    let store = ManagedNodeKeyStore(persistence: persistence) { count in
        [UInt8](repeating: 0x5a, count: count)
    }

    let migrated = try store.loadExisting()

    #expect(migrated.currentVersion == 4)
    #expect(migrated.entries().map(\.version) == [4])
    #expect(migrated.entries()[0].bytes == [UInt8](repeating: 0x61, count: 32))
    #expect(migrated.token.utf8.count == 48)
    #expect(migrated.token.utf8.allSatisfy { (0x21...0x7e).contains($0) })
    #expect(persistence.dataSnapshot?.starts(with: Data("CVSEC002".utf8)) == true)
    #expect(persistence.writeCount == 1)
}

@Test func corruptV2TokenFailsClosedWithoutReplacingTheKEKHierarchy() throws {
    var corrupted = legacyBytes(version: 1, key: 0x51)
    corrupted.replaceSubrange(0..<8, with: Array("CVSEC002".utf8))
    corrupted.insert(contentsOf: [0, 32], at: 14)
    corrupted.append(contentsOf: [UInt8](repeating: 0x20, count: 32))
    let persistence = TestManagedNodeKeyPersistence(data: Data(corrupted))
    let store = ManagedNodeKeyStore(persistence: persistence) { count in
        [UInt8](repeating: 0x42, count: count)
    }

    #expect(throws: ManagedNodeKeyStoreError.corruptHierarchy) {
        _ = try store.loadExisting()
    }
    #expect(persistence.writeCount == 0)
}

private let testTokenBytes = Array("test-local-api-token-with-at-least-thirty-two-bytes".utf8)

private func legacyBytes(version: UInt32, key: UInt8) -> [UInt8] {
    var result = Array("CVKEK001".utf8)
    result.append(contentsOf: withUnsafeBytes(of: version.bigEndian) { Array($0) })
    result.append(contentsOf: [0, 1])
    result.append(contentsOf: withUnsafeBytes(of: version.bigEndian) { Array($0) })
    result.append(contentsOf: [UInt8](repeating: key, count: 32))
    return result
}

private final class RacingManagedNodeKeyPersistence: ManagedNodeKeyPersisting, @unchecked Sendable {
    private let lock = NSLock()
    private let winner: Data
    private var data: Data?

    init(winner: Data) {
        self.winner = winner
    }

    func read() -> Data? {
        lock.withLock { data }
    }

    func insert(_: Data) -> Bool {
        lock.withLock {
            data = winner
            return false
        }
    }

    func update(_ data: Data) {
        lock.withLock { self.data = data }
    }
}

private final class TestManagedNodeKeyPersistence: ManagedNodeKeyPersisting, @unchecked Sendable {
    private let lock = NSLock()
    private var data: Data?
    private let error: ManagedNodeKeyStoreError?
    private(set) var writeCount = 0
    var dataSnapshot: Data? { lock.withLock { data } }

    init(data: Data? = nil, error: ManagedNodeKeyStoreError? = nil) {
        self.data = data
        self.error = error
    }

    func read() throws -> Data? {
        try lock.withLock {
            if let error { throw error }
            return data
        }
    }

    func insert(_ data: Data) throws -> Bool {
        try lock.withLock {
            if let error { throw error }
            guard self.data == nil else { return false }
            self.data = data
            writeCount += 1
            return true
        }
    }

    func update(_ data: Data) throws {
        try lock.withLock {
            if let error { throw error }
            guard self.data != nil else { throw ManagedNodeKeyStoreError.missingHierarchy }
            self.data = data
            writeCount += 1
        }
    }
}

private final class LockedByteCounter: @unchecked Sendable {
    private let lock = NSLock()
    private var value: UInt8 = 0

    func next() -> UInt8 {
        lock.withLock {
            value += 1
            return value
        }
    }
}
#endif
