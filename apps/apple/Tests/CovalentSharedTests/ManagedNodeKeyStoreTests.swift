#if os(macOS)
  import Darwin
  import Foundation
  import Security
  import Testing
  @testable import CovalentShared

  @Test func personalKeychainProvisioningUsesLoginAndReusesItsAtomicWinner() throws {
    let protected = TestManagedNodeKeyPersistence(error: .keychain(errSecMissingEntitlement))
    let login = TestManagedNodeKeyPersistence()
    let persistence = CompatibleManagedNodeKeyPersistence(
      dataProtection: protected, login: login, personalAdHocBuild: true,
      transactionLock: TestManagedNodeTransactionLock()
    )
    let bytes = Data([1, 2, 3])
    #expect(try persistence.read() == nil)
    #expect(try persistence.insert(bytes))
    #expect(try persistence.read() == bytes)
    #expect(try !persistence.insert(Data([4, 5, 6])))
    #expect(login.dataSnapshot == bytes)
    #expect(login.writeCount == 1)
    #expect(protected.writeCount == 0)
  }

  @Test func provisionedBuildCreatesProtectedItemAndPreservesPersonalUpgradeItem() throws {
    let protected = TestManagedNodeKeyPersistence()
    let login = TestManagedNodeKeyPersistence()
    let persistence = CompatibleManagedNodeKeyPersistence(
      dataProtection: protected, login: login, personalAdHocBuild: false,
      transactionLock: TestManagedNodeTransactionLock()
    )
    #expect(try persistence.insert(Data([1])))
    #expect(protected.writeCount == 1)
    #expect(login.writeCount == 0)

    let existingLogin = TestManagedNodeKeyPersistence(data: Data([2]))
    let absentProtected = TestManagedNodeKeyPersistence()
    let upgraded = CompatibleManagedNodeKeyPersistence(
      dataProtection: absentProtected, login: existingLogin, personalAdHocBuild: false,
      transactionLock: TestManagedNodeTransactionLock()
    )
    #expect(try upgraded.read() == Data([2]))
    #expect(try !upgraded.insert(Data([3])))
    try upgraded.update(Data([4]))
    #expect(existingLogin.dataSnapshot == Data([4]))
    #expect(absentProtected.writeCount == 0)
  }

  @Test func readableExistingProtectedHierarchyIsNeverCopiedToLogin() throws {
    let protected = TestManagedNodeKeyPersistence(data: Data([1]))
    let login = TestManagedNodeKeyPersistence()
    let persistence = CompatibleManagedNodeKeyPersistence(
      dataProtection: protected, login: login, personalAdHocBuild: true,
      transactionLock: TestManagedNodeTransactionLock()
    )
    #expect(try persistence.read() == Data([1]))
    #expect(try !persistence.insert(Data([2])))
    try persistence.update(Data([3]))
    #expect(protected.dataSnapshot == Data([3]))
    #expect(login.writeCount == 0)
  }

  @Test func missingProvisionedEntitlementNeverFallsBackToAnotherKeychain() throws {
    let protected = TestManagedNodeKeyPersistence(error: .keychain(errSecMissingEntitlement))
    let login = TestManagedNodeKeyPersistence(data: Data([1]))
    let persistence = CompatibleManagedNodeKeyPersistence(
      dataProtection: protected, login: login, personalAdHocBuild: false,
      transactionLock: TestManagedNodeTransactionLock()
    )
    #expect(throws: ManagedNodeKeyStoreError.keychain(errSecMissingEntitlement)) {
      _ = try persistence.read()
    }
    #expect(throws: ManagedNodeKeyStoreError.keychain(errSecMissingEntitlement)) {
      _ = try persistence.insert(Data([2]))
    }
    #expect(login.writeCount == 0)
  }

  @Test(arguments: [
    ManagedNodeKeyStoreError.keychainLocked, .corruptHierarchy, .keychain(errSecNotAvailable),
  ])
  func personalKeychainErrorsNeverCreateOrReplaceAnIdentity(error: ManagedNodeKeyStoreError) throws
  {
    let protected = TestManagedNodeKeyPersistence(error: error)
    let login = TestManagedNodeKeyPersistence()
    let persistence = CompatibleManagedNodeKeyPersistence(
      dataProtection: protected, login: login, personalAdHocBuild: true,
      transactionLock: TestManagedNodeTransactionLock()
    )
    #expect(throws: error) { _ = try persistence.insert(Data([2])) }
    #expect(login.writeCount == 0)
  }

  @Test func deniedPersonalUpgradeNeverCreatesASecondProtectedIdentity() throws {
    let protected = TestManagedNodeKeyPersistence()
    let login = TestManagedNodeKeyPersistence(error: .keychainLocked)
    let persistence = CompatibleManagedNodeKeyPersistence(
      dataProtection: protected, login: login, personalAdHocBuild: false,
      transactionLock: TestManagedNodeTransactionLock()
    )
    #expect(throws: ManagedNodeKeyStoreError.keychainLocked) {
      _ = try persistence.insert(Data([2]))
    }
    #expect(protected.writeCount == 0)
  }

  @Test func duplicateKeychainHierarchiesFailClosedEvenWhenTheirBytesMatch() throws {
    let protected = TestManagedNodeKeyPersistence(data: Data([1]))
    let login = TestManagedNodeKeyPersistence(data: Data([1]))
    let persistence = CompatibleManagedNodeKeyPersistence(
      dataProtection: protected, login: login, personalAdHocBuild: true,
      transactionLock: TestManagedNodeTransactionLock()
    )
    #expect(throws: ManagedNodeKeyStoreError.conflictingHierarchies) { _ = try persistence.read() }
    #expect(throws: ManagedNodeKeyStoreError.conflictingHierarchies) {
      _ = try persistence.insert(Data([2]))
    }
    #expect(throws: ManagedNodeKeyStoreError.conflictingHierarchies) {
      try persistence.update(Data([2]))
    }
    #expect(protected.writeCount == 0)
    #expect(login.writeCount == 0)
  }

  @Test func compatibleKeychainUpdateNeverCreatesAMissingHierarchy() throws {
    let protected = TestManagedNodeKeyPersistence()
    let login = TestManagedNodeKeyPersistence()
    let persistence = CompatibleManagedNodeKeyPersistence(
      dataProtection: protected, login: login, personalAdHocBuild: true,
      transactionLock: TestManagedNodeTransactionLock()
    )
    #expect(throws: ManagedNodeKeyStoreError.missingHierarchy) { try persistence.update(Data([2])) }
    #expect(protected.writeCount == 0)
    #expect(login.writeCount == 0)
  }

  @Test func differentlySignedFirstLaunchesShareOneAtomicKeychainWinner() throws {
    let protected = TestManagedNodeKeyPersistence()
    let login = TestManagedNodeKeyPersistence()
    let transactionLock = TestManagedNodeTransactionLock()
    let stores = [false, true].map {
      CompatibleManagedNodeKeyPersistence(
        dataProtection: protected, login: login, personalAdHocBuild: $0,
        transactionLock: transactionLock
      )
    }
    let results = LockedInsertionResults()
    DispatchQueue.concurrentPerform(iterations: 2) { index in
      do { results.record(try stores[index].insert(Data([UInt8(index + 1)]))) } catch {
        Issue.record(error)
      }
    }
    #expect(results.successes == 1)
    #expect(protected.writeCount + login.writeCount == 1)
    #expect(try stores[0].read() == stores[1].read())
  }

  @Test func privateKeychainFileLockExcludesAnotherHandleAndSurvivesUnlock() throws {
    let root = FileManager.default.temporaryDirectory.appending(path: "CovalentKeyLock-\(UUID())")
    try FileManager.default.createDirectory(at: root, withIntermediateDirectories: false)
    defer { try? FileManager.default.removeItem(at: root) }
    let first = ManagedNodeKeychainFileLock(service: "test", account: "fixture", home: root)
    let second = ManagedNodeKeychainFileLock(service: "test", account: "fixture", home: root)
    _ = try first.withLock {
      #expect(throws: ManagedNodeKeyStoreError.keychainBusy) { try second.withLock {} }
    }
    try second.withLock {}
    let directory = root.appending(path: "Library/Application Support/Covalent/KeychainLocks")
    let locks = try FileManager.default.contentsOfDirectory(
      at: directory, includingPropertiesForKeys: nil)
    #expect(locks.count == 1)
    #expect(try Data(contentsOf: #require(locks.first)).isEmpty)
  }

  @Test func keychainLockRejectsSymlinkedPrivateDirectoryBeforeOpeningRecords() throws {
    let root = FileManager.default.temporaryDirectory.appending(path: "CovalentKeyLock-\(UUID())")
    try FileManager.default.createDirectory(at: root, withIntermediateDirectories: false)
    defer { try? FileManager.default.removeItem(at: root) }
    let target = root.appending(path: "unrelated")
    try FileManager.default.createDirectory(at: target, withIntermediateDirectories: false)
    try FileManager.default.createSymbolicLink(
      at: root.appending(path: "Library"), withDestinationURL: target)
    let lock = ManagedNodeKeychainFileLock(service: "test", account: "fixture", home: root)
    #expect(throws: ManagedNodeKeyStoreError.keychainCoordinationUnavailable) {
      try lock.withLock {
        Issue.record("No Keychain access is allowed through a redirected lock path")
      }
    }
    #expect(try FileManager.default.contentsOfDirectory(atPath: target.path).isEmpty)
  }

  @Test func protectedKeyBytesAreErasedWhenTheSecondKeychainReadFails() throws {
    let erased = ErasedBufferRecorder()
    let persistence = CompatibleManagedNodeKeyPersistence(
      dataProtection: ErasureCheckingKeyPersistence(recorder: erased),
      login: TestManagedNodeKeyPersistence(error: .keychainLocked),
      personalAdHocBuild: true, transactionLock: TestManagedNodeTransactionLock()
    )
    #expect(throws: ManagedNodeKeyStoreError.keychainLocked) { _ = try persistence.read() }
    #expect(erased.results == [true])
  }

  @Test func conflictingKeychainBuffersAreBothErasedBeforeReturningTheError() throws {
    let erased = ErasedBufferRecorder()
    let persistence = CompatibleManagedNodeKeyPersistence(
      dataProtection: ErasureCheckingKeyPersistence(recorder: erased),
      login: ErasureCheckingKeyPersistence(recorder: erased),
      personalAdHocBuild: true, transactionLock: TestManagedNodeTransactionLock()
    )
    #expect(throws: ManagedNodeKeyStoreError.conflictingHierarchies) { _ = try persistence.read() }
    #expect(erased.results == [true, true])
  }

  @Test func inaccessibleProtectedIdentityCannotBeReplacedDuringPersonalDowngrade() throws {
    let login = TestManagedNodeKeyPersistence()
    let persistence = CompatibleManagedNodeKeyPersistence(
      dataProtection: TestManagedNodeKeyPersistence(error: .keychain(errSecMissingEntitlement)),
      login: login, personalAdHocBuild: true, transactionLock: TestManagedNodeTransactionLock()
    )
    let store = ManagedNodeKeyStore(persistence: persistence) { _ in
      Issue.record("Existing wrapped state must never generate a replacement hierarchy")
      return []
    }
    // LocalNodeManager uses loadExisting whenever protected node state exists.
    #expect(throws: ManagedNodeKeyStoreError.missingHierarchy) { _ = try store.loadExisting() }
    #expect(login.writeCount == 0)
  }

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
    #expect(
      ManagedNodeKeyStoreError.keychainLocked.localizedDescription.contains("Unlock this Mac"))
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

  @Test func recoveryPipeUsesDedicatedV3EnvelopeAndErasesEveryInput() throws {
    var descriptors = [Int32](repeating: -1, count: 2)
    #expect(Darwin.pipe(&descriptors) == 0)
    defer {
      Darwin.close(descriptors[0])
      Darwin.close(descriptors[1])
    }
    var material = try ManagedNodeKeyMaterial(
      currentVersion: 1,
      keys: [(1, [UInt8](repeating: 0x33, count: 32))],
      apiToken: testTokenBytes
    )
    var kit = Data("authenticated encrypted recovery kit".utf8)
    var code = Data(String(repeating: "A", count: 43).utf8)
    try material.writeRecoveryEnvelopeAndErase(kit: &kit, recoveryKey: &code, to: descriptors[1])
    #expect(material.bytes.allSatisfy { $0 == 0 })
    #expect(kit.isEmpty)
    #expect(code.isEmpty)

    var received = [UInt8](repeating: 0, count: 1_024)
    let count = received.withUnsafeMutableBytes {
      Darwin.read(descriptors[0], $0.baseAddress, $0.count)
    }
    #expect(count > 0)
    #expect(Array(received.prefix(8)) == Array("CVSEC003".utf8))
    #expect(!Array(received.prefix(Int(count))).starts(with: Array("CVSEC002".utf8)))
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
    let reconnect = try #require(
      manager.range(of: "reconnectToExistingConfiguration(paths: paths)"))
    let keyLoad = try #require(manager.range(of: "keyStore.loadOrCreate()"))

    #expect(
      reconnect.lowerBound < keyLoad.lowerBound,
      "Crash adoption must not depend on a fresh Keychain read")
    #expect(manager.contains("process.standardInput = keyPipe"))
    #expect(manager.contains("\"--key-encryption-key-stdin\""))
    #expect(manager.contains("writeRecoveryEnvelopeAndErase"))
    #expect(!manager.contains("\"--recovery-kit-file\""))
    #expect(!manager.contains("\"--recovery-key-file\""))
    #expect(manager.contains("keyMaterial.writeAndErase"))
    #expect(manager.contains("hasProtectedLocalSecret"))
    #expect(manager.contains("keyStore.loadExisting()"))
    #expect(!manager.contains("local-api-token"))
    #expect(manager.contains("process.environment = childEnvironment()"))
    #expect(manager.contains("COVALENT_SYNC_RUNTIME_DIR"))
    #expect(
      manager.contains("environment.removeValue(forKey: \"COVALENT_SYNC_ACCESS_UNAVAILABLE\")"))
    #expect(!manager.contains("COVALENT_KEY_ENCRYPTION_KEY="))
    #expect(!manager.contains("COVALENT_API_TOKEN="))
  }

  @Test func managedNodeUsesStableSignedPeerEndpointAcrossEveryLaunchMode() throws {
    let appleRoot = URL(fileURLWithPath: #filePath)
      .deletingLastPathComponent()
      .deletingLastPathComponent()
      .deletingLastPathComponent()
    let manager = try String(
      contentsOf: appleRoot.appending(path: "Sources/CovalentMac/LocalNodeManager.swift"),
      encoding: .utf8
    )

    #expect(manager.contains("private static let peerListenAddress = \"0.0.0.0:8787\""))
    #expect(manager.components(separatedBy: "\"--peer-listen\", Self.peerListenAddress").count == 3)
    #expect(!manager.contains("\"--peer-listen\", \"0.0.0.0:0\""))
  }

  @Test func managedNodeOpensTheExactResolvedSecurityScopedURL() throws {
    let appleRoot = URL(fileURLWithPath: #filePath)
      .deletingLastPathComponent()
      .deletingLastPathComponent()
      .deletingLastPathComponent()
    let manager = try String(
      contentsOf: appleRoot.appending(path: "Sources/CovalentMac/LocalNodeManager.swift"),
      encoding: .utf8
    )

    #expect(manager.contains("let url = try grant.resolve().url\n"))
    #expect(manager.contains("let standardizedPath = url.standardizedFileURL.path"))
    #expect(manager.contains("url.startAccessingSecurityScopedResource()"))
    #expect(!manager.contains("grant.resolve().url.standardizedFileURL"))
  }

  @Test func managedNodeShutdownRetainsIdentityUntilConfirmedExit() throws {
    let appleRoot = URL(fileURLWithPath: #filePath)
      .deletingLastPathComponent()
      .deletingLastPathComponent()
      .deletingLastPathComponent()
    let manager = try String(
      contentsOf: appleRoot.appending(path: "Sources/CovalentMac/LocalNodeManager.swift"),
      encoding: .utf8
    )
    let stop = try #require(manager.range(of: "private func stopManagedNode()"))
    let finish = try #require(manager.range(of: "private func finishManagedNodeStop("))
    let stopBody = manager[stop.lowerBound..<finish.lowerBound]

    #expect(stopBody.contains("guard !managedStopRequested else { return }"))
    #expect(stopBody.contains("guard process.isRunning else"))
    #expect(stopBody.contains("try await Task.sleep"))
    #expect(!stopBody.contains("ownedProcess = nil"))
    #expect(!stopBody.contains("managedProcessID = nil"))
    #expect(manager.contains("guard ownedProcess === expectedProcess"))
  }

  @Test func appleHarnessesNeverExtractADaemonTokenOrPassItAsLaunchMetadata() throws {
    let appleRoot = URL(fileURLWithPath: #filePath)
      .deletingLastPathComponent()
      .deletingLastPathComponent()
      .deletingLastPathComponent()
    for script in [
      "Scripts/integration-test.sh", "Scripts/macos-ui-test.sh", "Scripts/ios-ui-test.sh",
    ] {
      let source = try String(contentsOf: appleRoot.appending(path: script), encoding: .utf8)
      #expect(!source.contains("local-api-token"), "\(script) must not read a daemon token file")
      #expect(!source.contains("token=$(tr -d"), "\(script) must not extract a plaintext token")
      #expect(
        source.contains("--api-token-file"), "\(script) must use the owner-only harness input")
    }
  }

  @Test func appleUITestHarnessUsesOnlyAPrivateTokenPathAndReleaseBuildsIgnoreIt() throws {
    let appleRoot = URL(fileURLWithPath: #filePath)
      .deletingLastPathComponent()
      .deletingLastPathComponent()
      .deletingLastPathComponent()
    for script in ["Scripts/macos-ui-test.sh", "Scripts/ios-ui-test.sh"] {
      let source = try String(contentsOf: appleRoot.appending(path: script), encoding: .utf8)
      #expect(
        source.contains("COVALENT_UI_TEST_TOKEN_FILE"), "\(script) must pass only a token-file path"
      )
      #expect(
        source.contains("copy-owner-only-token.py"),
        "\(script) must provision the target-app private file")
      #expect(
        source.contains("ui-token-$token_nonce"), "\(script) must use a unique relative filename")
      #expect(
        source.contains("rm -f -- \"$app_token_file\""),
        "\(script) must remove only its own token file")
      #expect(
        !source.contains("COVALENT_UI_TEST_TOKEN ="),
        "\(script) must not write raw token build settings")
    }

    let project = try String(contentsOf: appleRoot.appending(path: "Project.yml"), encoding: .utf8)
    let tokenFileSetting = "CovalentUITestTokenFile: $(COVALENT_UI_TEST_TOKEN_FILE)"
    #expect(
      project.components(separatedBy: tokenFileSetting).count == 3,
      "Both UI-test targets must receive only the private token-file path")
    #expect(!project.contains("CovalentUITestToken: $(COVALENT_UI_TEST_TOKEN)"))
    for plist in ["Config/CovalentMacUITests-Info.plist", "Config/CovalentIOSUITests-Info.plist"] {
      #expect(project.contains("path: \(plist)"), "Project.yml must generate \(plist)")
    }

    let model = try String(
      contentsOf: appleRoot.appending(path: "Sources/CovalentShared/CovalentAppModel.swift"),
      encoding: .utf8
    )
    #expect(
      model.contains("#if DEBUG\n            let environment = ProcessInfo.processInfo.environment")
    )
    #expect(model.contains("O_NOFOLLOW"), "UI-test token reads must reject symlinks")
    #expect(model.contains("metadata.st_uid == getuid()"))
    #expect(model.contains("mode_t(0o600)"))
    #expect(
      model.contains(
        "#else\n            loadedConfiguration = (try? connectionStore.load()) ?? .localDefault"),
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

  @Test func independentStoresSerializeCompleteKeyRotationTransactions() throws {
    let initial = try ManagedNodeKeyMaterial(
      currentVersion: 1,
      keys: [(1, [UInt8](repeating: 0x41, count: ManagedNodeKeyMaterial.keyLength))],
      apiToken: testTokenBytes
    )
    let persistence = TestManagedNodeKeyPersistence(data: Data(initial.bytes))
    let transactionLock = TestManagedNodeTransactionLock()
    let random = LockedByteCounter()
    let stores = (0..<2).map { _ in
      ManagedNodeKeyStore(persistence: persistence, transactionLock: transactionLock) { count in
        [UInt8](repeating: random.next(), count: count)
      }
    }
    let results = LockedKeyOperationResults()

    DispatchQueue.concurrentPerform(iterations: stores.count) { index in
      do {
        let rotated = try stores[index].rotate()
        results.record(version: rotated.currentVersion, token: rotated.token)
      } catch {
        Issue.record(error)
      }
    }

    #expect(results.versions.sorted() == [2, 3])
    #expect(Set(results.tokens).count == 1)
    #expect(transactionLock.entryCount == 2)
    #expect(persistence.writeCount == 2)
    let reopened = try stores[0].loadExisting()
    #expect(reopened.currentVersion == 3)
    #expect(reopened.entries().map(\.version) == [1, 2, 3])
  }

  @Test func independentFileLocksPreventAStaleRotationReadBeforeKeyGeneration() throws {
    let root = FileManager.default.temporaryDirectory.appending(
      path: "CovalentKeyTransaction-\(UUID())")
    try FileManager.default.createDirectory(at: root, withIntermediateDirectories: false)
    defer { try? FileManager.default.removeItem(at: root) }
    let initial = try ManagedNodeKeyMaterial(
      currentVersion: 1,
      keys: [(1, [UInt8](repeating: 0x41, count: ManagedNodeKeyMaterial.keyLength))],
      apiToken: testTokenBytes
    )
    let persistence = TestManagedNodeKeyPersistence(data: Data(initial.bytes))
    let firstEnteredRandomGeneration = DispatchSemaphore(value: 0)
    let releaseFirstRotation = DispatchSemaphore(value: 0)
    let firstResults = LockedKeyOperationResults()
    let first = ManagedNodeKeyStore(
      persistence: persistence,
      transactionLock: ManagedNodeKeychainFileLock(
        service: "test", account: "rotation", home: root)
    ) { count in
      firstEnteredRandomGeneration.signal()
      guard releaseFirstRotation.wait(timeout: .now() + 5) == .success else {
        throw ManagedNodeKeyStoreError.randomGenerationFailed
      }
      return [UInt8](repeating: 0x51, count: count)
    }
    let secondRandom = LockedByteCounter()
    let second = ManagedNodeKeyStore(
      persistence: persistence,
      transactionLock: ManagedNodeKeychainFileLock(
        service: "test", account: "rotation", home: root)
    ) { count in
      [UInt8](repeating: secondRandom.next(), count: count)
    }
    let firstFinished = DispatchGroup()
    firstFinished.enter()
    // This operation intentionally blocks while holding the file lock. Give it
    // a dedicated thread so parallel tests cannot starve its readiness signal.
    let firstThread = Thread {
      defer { firstFinished.leave() }
      do {
        let rotated = try first.rotate()
        firstResults.record(version: rotated.currentVersion, token: rotated.token)
      } catch {
        Issue.record(error)
      }
    }
    firstThread.start()
    defer {
      releaseFirstRotation.signal()
      firstFinished.wait()
    }

    try #require(firstEnteredRandomGeneration.wait(timeout: .now() + 5) == .success)
    #expect(throws: ManagedNodeKeyStoreError.keychainBusy) { _ = try second.rotate() }
    #expect(secondRandom.current == 0)
    releaseFirstRotation.signal()
    try #require(firstFinished.wait(timeout: .now() + 5) == .success)
    #expect(firstResults.versions == [2])

    let final = try second.rotate()
    #expect(final.currentVersion == 3)
    #expect(final.entries().map(\.version) == [1, 2, 3])
    #expect(final.token == firstResults.tokens.first)
    #expect(secondRandom.current == 1)
  }

  @Test func legacyMigrationAndRotationCannotOverwriteOneAnother() throws {
    let persistence = TestManagedNodeKeyPersistence(data: Data(legacyBytes(version: 4, key: 0x61)))
    let transactionLock = TestManagedNodeTransactionLock()
    let random = LockedByteCounter()
    let stores = (0..<2).map { _ in
      ManagedNodeKeyStore(persistence: persistence, transactionLock: transactionLock) { count in
        [UInt8](repeating: random.next(), count: count)
      }
    }
    let results = LockedKeyOperationResults()

    DispatchQueue.concurrentPerform(iterations: stores.count) { index in
      do {
        let material = try index == 0 ? stores[index].loadExisting() : stores[index].rotate()
        results.record(version: material.currentVersion, token: material.token)
      } catch {
        Issue.record(error)
      }
    }

    #expect(results.versions.count == 2)
    #expect(results.versions.allSatisfy { $0 == 4 || $0 == 5 })
    #expect(results.versions.contains(5))
    #expect(Set(results.tokens).count == 1)
    #expect(transactionLock.entryCount == 2)
    #expect(persistence.writeCount == 2)
    let reopened = try stores[0].loadExisting()
    #expect(reopened.currentVersion == 5)
    #expect(reopened.entries().map(\.version) == [4, 5])
    let expectedToken = try #require(results.tokens.first)
    #expect(reopened.token == expectedToken)
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

  private final class RacingManagedNodeKeyPersistence: ManagedNodeKeyPersisting, @unchecked Sendable
  {
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

  private final class LockedInsertionResults: @unchecked Sendable {
    private let lock = NSLock()
    private var values: [Bool] = []
    var successes: Int { lock.withLock { values.filter { $0 }.count } }
    func record(_ value: Bool) { lock.withLock { values.append(value) } }
  }

  private final class ErasedBufferRecorder: @unchecked Sendable {
    private let lock = NSLock()
    private var values: [Bool] = []
    var results: [Bool] { lock.withLock { values } }
    func record(_ value: Bool) { lock.withLock { values.append(value) } }
  }

  private struct ErasureCheckingKeyPersistence: ManagedNodeKeyPersisting {
    let recorder: ErasedBufferRecorder
    func read() throws -> Data? {
      let count = 64
      let pointer = try #require(malloc(count))
      memset(pointer, 0x5a, count)
      return Data(
        bytesNoCopy: pointer, count: count,
        deallocator: .custom { pointer, count in
          recorder.record(
            UnsafeRawBufferPointer(start: pointer, count: count).allSatisfy { $0 == 0 })
          free(pointer)
        })
    }
    func insert(_ data: Data) throws -> Bool { throw ManagedNodeKeyStoreError.corruptHierarchy }
    func update(_ data: Data) throws { throw ManagedNodeKeyStoreError.corruptHierarchy }
  }

  private final class TestManagedNodeTransactionLock: ManagedNodeKeyTransactionLock,
    @unchecked Sendable
  {
    private let lock = NSLock()
    private var entries = 0
    var entryCount: Int { lock.withLock { entries } }

    func withLock<T: ~Copyable>(_ operation: () throws -> T) throws -> T {
      lock.lock()
      entries += 1
      defer { lock.unlock() }
      return try operation()
    }
  }

  private final class LockedKeyOperationResults: @unchecked Sendable {
    private let lock = NSLock()
    private var recordedVersions: [UInt32] = []
    private var recordedTokens: [String] = []
    var versions: [UInt32] { lock.withLock { recordedVersions } }
    var tokens: [String] { lock.withLock { recordedTokens } }

    func record(version: UInt32, token: String) {
      lock.withLock {
        recordedVersions.append(version)
        recordedTokens.append(token)
      }
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
    var current: UInt8 { lock.withLock { value } }

    func next() -> UInt8 {
      lock.withLock {
        value += 1
        return value
      }
    }
  }
#endif
