import AppKit
import Darwin
import Foundation

@MainActor
final class LocalNodeManager: LocalNodeBootstrapping {
    private static let startupTimeout: Duration = .seconds(15)
    private static let existingHealthTimeout: Duration = .seconds(3)
    private static let shutdownTimeout: Duration = .seconds(5)
    private static let maximumLogBytes: UInt64 = 1_048_576

    private let fileManager: FileManager
    private let session: URLSession
    private let keyStore: ManagedNodeKeyStore
    private var ownedProcess: Process?
    private var managedProcessID: Int32?
    private var managedReadyFile: URL?
    private var managedStopRequested = false
    /// Exists only while this app process owns or has adopted the local node.
    /// It is never written to the node data directory or launch metadata.
    private var managedAPIToken: String?
    private var logHandle: FileHandle?
    private var terminationObserver: NSObjectProtocol?
    /// Security-scoped roots stay active for the complete helper lifetime so
    /// its inherited sandbox and both maintained-engine descendants retain
    /// only the folders the user selected.
    private var heldFolderSyncDirectories: [URL] = []
    private var folderSyncAccessUnavailable = false

    init(
        fileManager: FileManager = .default,
        session: URLSession? = nil,
        keyStore: ManagedNodeKeyStore = ManagedNodeKeyStore()
    ) {
        self.fileManager = fileManager
        self.keyStore = keyStore
        if let session {
            self.session = session
        } else {
            let configuration = URLSessionConfiguration.ephemeral
            configuration.requestCachePolicy = .reloadIgnoringLocalCacheData
            configuration.timeoutIntervalForRequest = 2
            configuration.urlCache = nil
            self.session = URLSession(configuration: configuration)
        }
        terminationObserver = NotificationCenter.default.addObserver(
            forName: NSApplication.willTerminateNotification,
            object: nil,
            queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated {
                self?.stopManagedNode()
            }
        }
    }

    func startupDisposition() throws -> LocalNodeStartupDisposition {
        let paths = try managedPaths(createApplicationSupport: false)
        var metadata = stat()
        if Darwin.lstat(paths.dataDirectory.path, &metadata) != 0 {
            guard errno == ENOENT else { throw LocalNodeError.insecurePrivateFile }
            return .needsFirstLaunchChoice
        }
        guard metadata.st_mode & S_IFMT == S_IFDIR,
              metadata.st_uid == getuid(),
              metadata.st_mode & 0o077 == 0
        else { return .existingIdentity }

        // A normal root is fresh only when it is literally empty. This keeps
        // recovery from deleting, replacing, or interpreting any unrelated
        // durable state, including a failed recovery journal.
        let entries = try fileManager.contentsOfDirectory(
            at: paths.dataDirectory,
            includingPropertiesForKeys: nil,
            options: []
        )
        guard !entries.isEmpty else { return .needsFirstLaunchChoice }
        let names = Set(entries.map(\.lastPathComponent))
        if names.contains("recovery-bootstrap.json"), names.contains("identity.json") {
            return .resumableRecovery
        }
        return .existingIdentity
    }

    func start(mode: LocalNodeStartupMode) async throws -> NodeConnectionConfiguration {
        switch mode {
        case .normal:
            return try await startNormal()
        case let .recover(recoveryKitFile, recoveryKeyFile):
            return try await startRecovery(
                recoveryKitFile: recoveryKitFile,
                recoveryKeyFile: recoveryKeyFile
            )
        }
    }

    /// Resolve and retain every saved folder-sync bookmark before any helper
    /// launch. A stale or revoked grant disables folder sync as one complete
    /// set while allowing the backup and recovery service to start.
    func prepareFolderSyncDirectoryGrants(
        _ grants: [SelectedDirectoryGrant]
    ) async throws {
        let prepared: [URL]
        do {
            prepared = try startFolderSyncScopes(grants)
        } catch {
            // On cold app startup the previous app-owned helper can still be
            // serving. Adopt only its private ready record, then stop and reap
            // it before starting the backup-only service. Folder sync remains
            // visibly unavailable until the complete grant set is restored.
            if managedProcessID == nil,
               let paths = try? managedPaths(createApplicationSupport: false) {
                _ = try? await reconnectToExistingConfiguration(paths: paths)
            }
            try await stopManagedNodeAndWait()
            replaceFolderSyncScopes(with: [])
            folderSyncAccessUnavailable = true
            return
        }
        if sameFolderSyncScopes(prepared, heldFolderSyncDirectories) {
            stopFolderSyncScopes(prepared)
            folderSyncAccessUnavailable = false
            return
        }
        if managedProcessID == nil,
           let paths = try? managedPaths(createApplicationSupport: false) {
            _ = try? await reconnectToExistingConfiguration(paths: paths)
        }
        do {
            try await stopManagedNodeAndWait()
        } catch {
            stopFolderSyncScopes(prepared)
            throw error
        }
        replaceFolderSyncScopes(with: prepared)
        folderSyncAccessUnavailable = false
    }

    /// Replace the complete saved grant set and restart the helper so the new
    /// sandbox extensions are inherited before a folder offer is submitted.
    func restartForFolderSyncDirectoryGrants(
        _ grants: [SelectedDirectoryGrant]
    ) async throws -> NodeConnectionConfiguration {
        let prepared: [URL]
        do {
            prepared = try startFolderSyncScopes(grants)
        } catch {
            // Existing shares must not keep running after their complete
            // capability set can no longer be restored. Relaunch backup-only;
            // the folder mutation will receive a fixed unavailable response.
            try await stopManagedNodeAndWait()
            replaceFolderSyncScopes(with: [])
            folderSyncAccessUnavailable = true
            return try await startNormal()
        }
        do {
            try await stopManagedNodeAndWait()
        } catch {
            stopFolderSyncScopes(prepared)
            throw error
        }
        replaceFolderSyncScopes(with: prepared)
        folderSyncAccessUnavailable = false
        return try await startNormal()
    }

    private func startNormal() async throws -> NodeConnectionConfiguration {
        if managedStopRequested {
            try await stopManagedNodeAndWait()
        }
        let paths = try managedPaths()
        try preparePrivateDirectory(paths.dataDirectory)

        if let configuration = try await reconnectToExistingConfiguration(paths: paths) {
            return configuration
        }

        try await stopManagedNodeAndWait()
        try removeStaleReadyFile(paths.readyFile)
        let process = try launchNode(paths: paths)
        ownedProcess = process
        managedProcessID = process.processIdentifier
        managedReadyFile = paths.readyFile
        managedStopRequested = false

        let clock = ContinuousClock()
        let deadline = clock.now.advanced(by: Self.startupTimeout)
        while clock.now < deadline {
            if !process.isRunning {
                let details = readLogTail(paths.logFile)
                try await stopManagedNodeAndWait()
                throw LocalNodeError.exitedDuringStartup(details)
            }
            if let configuration = try await healthyExistingConfiguration(paths: paths),
               let ready = try? readReadyFile(paths.readyFile),
               ready.processId == process.processIdentifier {
                return configuration
            }
            try await Task.sleep(for: .milliseconds(100))
        }

        let details = readLogTail(paths.logFile)
        try await stopManagedNodeAndWait()
        throw LocalNodeError.startupTimedOut(details)
    }

    private func startRecovery(
        recoveryKitFile: URL,
        recoveryKeyFile: URL
    ) async throws -> NodeConnectionConfiguration {
        let paths = try managedPaths(createApplicationSupport: false)
        let disposition = try startupDisposition()
        guard disposition == .needsFirstLaunchChoice || disposition == .resumableRecovery else {
            throw LocalNodeError.recoveryTargetNotFresh
        }
        guard fileManager.isExecutableFile(atPath: paths.helper.path) else {
            throw LocalNodeError.helperMissing(paths.helper.path)
        }

        var kit = try readPrivateRecoveryFile(recoveryKitFile, maximumBytes: 16 * 1_024 * 1_024)
        var key = Data()
        defer { Self.erase(&kit); Self.erase(&key) }
        key = try readPrivateRecoveryFile(recoveryKeyFile, maximumBytes: 64)
        if key.last == 10 { key.removeLast() }
        if key.last == 13 { key.removeLast() }
        guard key.count == 43,
              key.allSatisfy({ byte in
                  (48...57).contains(byte) || (65...90).contains(byte)
                      || (97...122).contains(byte) || byte == 45 || byte == 95
              })
        else { throw LocalNodeError.invalidRecoveryMaterial }

        // Creating the parent is safe: the normal `Node` root remains absent
        // until the recovery engine publishes the authenticated identity.
        let recoveryLog = try recoveryLogFile(paths: paths)
        let logHandle = try FileHandle(forWritingTo: recoveryLog)
        try logHandle.seekToEnd()
        let process = Process()
        let keyPipe = Pipe()
        process.executableURL = paths.helper
        process.arguments = [
            "recover",
            "--listen", "127.0.0.1:0",
            "--peer-listen", "0.0.0.0:0",
            "--data-dir", paths.dataDirectory.path,
            "--device-name", Host.current().localizedName ?? "This Mac",
            "--lan-discovery",
            "--platform-tier", "tier1",
            "--ready-file", paths.readyFile.path,
            "--key-encryption-key-stdin",
        ]
        process.environment = childEnvironment()
        process.standardInput = keyPipe
        process.standardOutput = logHandle
        process.standardError = logHandle
        process.terminationHandler = { _ in }
        var material = try keyStore.loadOrCreate()
        let apiToken = material.token
        do {
            try process.run()
            try keyPipe.fileHandleForReading.close()
            defer { try? keyPipe.fileHandleForWriting.close() }
            try material.writeRecoveryEnvelopeAndErase(
                kit: &kit,
                recoveryKey: &key,
                to: keyPipe.fileHandleForWriting.fileDescriptor
            )
            managedAPIToken = apiToken
        } catch {
            material.erase()
            try? keyPipe.fileHandleForReading.close()
            try? keyPipe.fileHandleForWriting.close()
            if process.isRunning { process.terminate() }
            try? logHandle.close()
            throw LocalNodeError.launchFailed(error.localizedDescription)
        }
        ownedProcess = process
        managedProcessID = process.processIdentifier
        managedReadyFile = paths.readyFile
        managedStopRequested = false
        self.logHandle = logHandle

        let clock = ContinuousClock()
        let deadline = clock.now.advanced(by: Self.startupTimeout)
        while clock.now < deadline {
            if !process.isRunning {
                let details = readLogTail(recoveryLog)
                try await stopManagedNodeAndWait()
                throw LocalNodeError.exitedDuringStartup(details)
            }
            if let configuration = try await healthyExistingConfiguration(paths: paths),
               let ready = try? readReadyFile(paths.readyFile),
               ready.processId == process.processIdentifier {
                return configuration
            }
            try await Task.sleep(for: .milliseconds(100))
        }
        let details = readLogTail(recoveryLog)
        try await stopManagedNodeAndWait()
        throw LocalNodeError.startupTimedOut(details)
    }

    private func reconnectToExistingConfiguration(paths: ManagedPaths) async throws -> NodeConnectionConfiguration? {
        guard fileManager.fileExists(atPath: paths.readyFile.path) else { return nil }
        let ready = try readReadyFile(paths.readyFile)
        guard ready.schemaVersion == 1, ready.processId > 0 else {
            throw LocalNodeError.invalidReadyFile
        }
        if Darwin.kill(ready.processId, 0) != 0 {
            guard errno == ESRCH else { throw LocalNodeError.existingServiceUnavailable }
            return nil
        }
        let clock = ContinuousClock()
        let deadline = clock.now.advanced(by: Self.existingHealthTimeout)
        while clock.now < deadline {
            if let configuration = try await healthyExistingConfiguration(paths: paths) {
                return configuration
            }
            if Darwin.kill(ready.processId, 0) != 0 {
                guard errno == ESRCH else { throw LocalNodeError.existingServiceUnavailable }
                return nil
            }
            try await Task.sleep(for: .milliseconds(100))
        }
        throw LocalNodeError.existingServiceUnavailable
    }

    private func launchNode(paths: ManagedPaths) throws -> Process {
        guard fileManager.isExecutableFile(atPath: paths.helper.path) else {
            throw LocalNodeError.helperMissing(paths.helper.path)
        }
        try rotateLogIfNeeded(paths.logFile)
        if !fileManager.fileExists(atPath: paths.logFile.path) {
            guard fileManager.createFile(
                atPath: paths.logFile.path,
                contents: nil,
                attributes: [.posixPermissions: 0o600]
            ) else {
                throw LocalNodeError.logUnavailable(paths.logFile.path)
            }
        }
        let logHandle = try FileHandle(forWritingTo: paths.logFile)
        try logHandle.seekToEnd()

        let process = Process()
        let keyPipe = Pipe()
        process.executableURL = paths.helper
        process.arguments = [
            "serve",
            "--listen", "127.0.0.1:0",
            "--peer-listen", "0.0.0.0:0",
            "--data-dir", paths.dataDirectory.path,
            "--device-name", Host.current().localizedName ?? "This Mac",
            "--lan-discovery",
            "--platform-tier", "tier1",
            "--ready-file", paths.readyFile.path,
            "--key-encryption-key-stdin",
        ]
        process.environment = childEnvironment()
        process.standardInput = keyPipe
        process.standardOutput = logHandle
        process.standardError = logHandle
        process.terminationHandler = { _ in }
        var keyMaterial = try loadKeyMaterial(for: paths.dataDirectory)
        let apiToken = keyMaterial.token
        do {
            try process.run()
            try keyPipe.fileHandleForReading.close()
            defer { try? keyPipe.fileHandleForWriting.close() }
            try keyMaterial.writeAndErase(to: keyPipe.fileHandleForWriting.fileDescriptor)
            managedAPIToken = apiToken
        } catch {
            keyMaterial.erase()
            try? keyPipe.fileHandleForReading.close()
            try? keyPipe.fileHandleForWriting.close()
            if process.isRunning {
                process.terminate()
            }
            try? logHandle.close()
            throw LocalNodeError.launchFailed(error.localizedDescription)
        }
        self.logHandle = logHandle
        return process
    }

    private func loadKeyMaterial(for dataDirectory: URL) throws -> ManagedNodeKeyMaterial {
        if hasProtectedLocalSecret(in: dataDirectory) {
            return try keyStore.loadExisting()
        }
        return try keyStore.loadOrCreate()
    }

    /// Missing Keychain data may be provisioned for a first run or a legacy
    /// plaintext migration, but never over an already-wrapped local identity.
    private func hasProtectedLocalSecret(in dataDirectory: URL) -> Bool {
        let records = [
            dataDirectory.appending(path: "identity.json"),
            dataDirectory.appending(path: "tls/identity.json"),
        ]
        for record in records where fileManager.fileExists(atPath: record.path) {
            guard let data = try? Data(contentsOf: record, options: .uncached),
                  data.count <= 128 * 1_024,
                  let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
                  let schemaVersion = object["schemaVersion"] as? NSNumber
            else {
                return true
            }
            if schemaVersion.uintValue >= 2 || object["protectedPrivateKey"] != nil {
                return true
            }
            guard schemaVersion.uintValue == 1, object["privateKey"] != nil else {
                return true
            }
        }
        return false
    }

    private func healthyExistingConfiguration(paths: ManagedPaths) async throws -> NodeConnectionConfiguration? {
        guard fileManager.fileExists(atPath: paths.readyFile.path) else {
            return nil
        }
        let ready: ManagedNodeReady
        do {
            ready = try readReadyFile(paths.readyFile)
        } catch {
            return nil
        }
        guard ready.schemaVersion == 1,
              ready.processId > 0,
              Darwin.kill(ready.processId, 0) == 0,
              let baseURL = URL(string: ready.apiBaseUrl),
              baseURL.scheme?.lowercased() == "http",
              baseURL.host == "127.0.0.1",
              baseURL.port != nil,
              baseURL.path.isEmpty || baseURL.path == "/"
        else {
            return nil
        }
        guard try isPrivateRegularFile(paths.readyFile) else {
            throw LocalNodeError.insecurePrivateFile
        }
        guard await isHealthy(baseURL: baseURL) else { return nil }
        // Probe readiness before touching Keychain. A crash-adopted node that
        // is gone must not turn a transient locked Keychain into a start error.
        let token: String
        if let managedAPIToken {
            token = managedAPIToken
        } else {
            var material = try keyStore.loadExisting()
            token = material.token
            material.erase()
            managedAPIToken = token
        }
        let configuration = try NodeConnectionConfiguration(baseURL: baseURL, apiToken: token)
        managedProcessID = ready.processId
        managedReadyFile = paths.readyFile
        managedStopRequested = false
        return configuration
    }

    private func isHealthy(baseURL: URL) async -> Bool {
        var request = URLRequest(url: baseURL.appending(path: "healthz"))
        request.cachePolicy = .reloadIgnoringLocalCacheData
        request.timeoutInterval = 2
        do {
            let (_, response) = try await session.data(for: request)
            return (response as? HTTPURLResponse)?.statusCode == 200
        } catch {
            return false
        }
    }

    private func managedPaths(createApplicationSupport: Bool = true) throws -> ManagedPaths {
        guard let executableDirectory = Bundle.main.executableURL?.deletingLastPathComponent() else {
            throw LocalNodeError.helperMissing("Covalent.app/Contents/MacOS/covalent-node")
        }
        let dataDirectory: URL
        #if DEBUG
        if let override = ProcessInfo.processInfo.environment["COVALENT_MANAGED_NODE_DATA_DIR"],
           override.hasPrefix("/"),
           override != "/" {
            dataDirectory = URL(fileURLWithPath: override, isDirectory: true).standardizedFileURL
        } else {
            dataDirectory = try defaultDataDirectory(create: createApplicationSupport)
        }
        #else
            dataDirectory = try defaultDataDirectory(create: createApplicationSupport)
        #endif
        return ManagedPaths(
            helper: executableDirectory.appending(path: "covalent-node"),
            dataDirectory: dataDirectory,
            readyFile: dataDirectory.appending(path: "node-ready.json"),
            logFile: dataDirectory.appending(path: "node.log")
        )
    }

    private func defaultDataDirectory(create: Bool) throws -> URL {
        let applicationSupport: URL
        if create {
            applicationSupport = try fileManager.url(
                for: .applicationSupportDirectory,
                in: .userDomainMask,
                appropriateFor: nil,
                create: true
            )
        } else if let existing = fileManager.urls(
            for: .applicationSupportDirectory,
            in: .userDomainMask
        ).first {
            applicationSupport = existing
        } else {
            throw LocalNodeError.insecurePrivateFile
        }
        return applicationSupport
            .appending(path: "Covalent", directoryHint: .isDirectory)
            .appending(path: "Node", directoryHint: .isDirectory)
    }

    private func recoveryLogFile(paths: ManagedPaths) throws -> URL {
        let parent = paths.dataDirectory.deletingLastPathComponent()
        try preparePrivateDirectory(parent)
        let file = parent.appending(path: "node-recovery.log")
        if !fileManager.fileExists(atPath: file.path) {
            guard fileManager.createFile(
                atPath: file.path,
                contents: nil,
                attributes: [.posixPermissions: 0o600]
            ) else { throw LocalNodeError.logUnavailable(file.path) }
        }
        guard try isPrivateRegularFile(file) else { throw LocalNodeError.insecurePrivateFile }
        try rotateLogIfNeeded(file)
        return file
    }

    /// A user explicitly chooses this file, so require its owner and a
    /// no-follow regular descriptor but do not chmod a downloaded file in
    /// place. Exports are always 0600; imports may be 0644 and are warned
    /// about in the UI rather than requiring a Terminal workaround.
    private func readPrivateRecoveryFile(_ file: URL, maximumBytes: Int) throws -> Data {
        guard file.isFileURL, maximumBytes > 0 else { throw LocalNodeError.invalidRecoveryMaterial }
        let descriptor = Darwin.open(file.path, O_RDONLY | O_NOFOLLOW | O_CLOEXEC)
        guard descriptor >= 0 else { throw LocalNodeError.invalidRecoveryMaterial }
        defer { Darwin.close(descriptor) }
        var metadata = stat()
        guard Darwin.fstat(descriptor, &metadata) == 0,
              metadata.st_mode & S_IFMT == S_IFREG,
              metadata.st_uid == getuid(),
              metadata.st_size > 0
        else { throw LocalNodeError.invalidRecoveryMaterial }
        let handle = FileHandle(fileDescriptor: descriptor, closeOnDealloc: false)
        var data = Data()
        do {
            while true {
                let remaining = maximumBytes + 1 - data.count
                guard remaining > 0 else { throw LocalNodeError.invalidRecoveryMaterial }
                let chunk = try handle.read(upToCount: min(64 * 1_024, remaining)) ?? Data()
                if chunk.isEmpty { break }
                data.append(chunk)
                if data.count > maximumBytes { throw LocalNodeError.invalidRecoveryMaterial }
            }
            guard !data.isEmpty else { throw LocalNodeError.invalidRecoveryMaterial }
            return data
        } catch {
            Self.erase(&data)
            throw error
        }
    }

    private static func erase(_ data: inout Data) {
        data.withUnsafeMutableBytes { buffer in
            guard let base = buffer.baseAddress else { return }
            bzero(base, buffer.count)
        }
        data.removeAll(keepingCapacity: false)
    }

    private func preparePrivateDirectory(_ directory: URL) throws {
        try fileManager.createDirectory(at: directory, withIntermediateDirectories: true)
        try fileManager.setAttributes([.posixPermissions: 0o700], ofItemAtPath: directory.path)
    }

    private func childEnvironment() -> [String: String] {
        let runtimeDirectory = fileManager.temporaryDirectory
            .appending(path: "cvs", directoryHint: .isDirectory)
            .standardizedFileURL
        var environment = ProcessInfo.processInfo.environment
        // Rust performs the no-follow owner/mode/path-bound admission. A bad
        // native hint disables only folder sync; it must never block the
        // backup and recovery service from launching.
        if runtimeDirectory.path.utf8.count <= 900 {
            environment["COVALENT_SYNC_RUNTIME_DIR"] = runtimeDirectory.path
        } else {
            environment.removeValue(forKey: "COVALENT_SYNC_RUNTIME_DIR")
        }
        if folderSyncAccessUnavailable {
            environment["COVALENT_SYNC_ACCESS_UNAVAILABLE"] = "1"
        } else {
            environment.removeValue(forKey: "COVALENT_SYNC_ACCESS_UNAVAILABLE")
        }
        return environment
    }

    private func startFolderSyncScopes(
        _ grants: [SelectedDirectoryGrant]
    ) throws -> [URL] {
        let selected = grants.filter { $0.purpose == .folderSync }
        guard selected.count <= 128 else { throw LocalNodeError.insecurePrivateFile }
        var prepared: [URL] = []
        var paths = Set<String>()
        do {
            for grant in selected {
                let url = try grant.resolve().url.standardizedFileURL
                guard paths.insert(url.path).inserted else { continue }
                guard url.startAccessingSecurityScopedResource() else {
                    throw SelectedDirectoryError.accessDenied
                }
                prepared.append(url)
                let values = try url.resourceValues(forKeys: [.isDirectoryKey])
                guard values.isDirectory == true else {
                    throw SelectedDirectoryError.permissionRevoked
                }
            }
            return prepared
        } catch {
            stopFolderSyncScopes(prepared)
            throw error
        }
    }

    private func replaceFolderSyncScopes(with replacement: [URL]) {
        let previous = heldFolderSyncDirectories
        heldFolderSyncDirectories = replacement
        stopFolderSyncScopes(previous)
    }

    private func sameFolderSyncScopes(_ left: [URL], _ right: [URL]) -> Bool {
        Set(left.map { $0.standardizedFileURL.path })
            == Set(right.map { $0.standardizedFileURL.path })
    }

    private func stopFolderSyncScopes(_ directories: [URL]) {
        for directory in directories {
            directory.stopAccessingSecurityScopedResource()
        }
    }

    private func readReadyFile(_ file: URL) throws -> ManagedNodeReady {
        guard try isPrivateRegularFile(file) else { throw LocalNodeError.insecurePrivateFile }
        let data = try Data(contentsOf: file, options: .uncached)
        guard data.count <= 4_096 else { throw LocalNodeError.invalidReadyFile }
        return try JSONDecoder().decode(ManagedNodeReady.self, from: data)
    }

    private func isPrivateRegularFile(_ file: URL) throws -> Bool {
        let attributes = try fileManager.attributesOfItem(atPath: file.path)
        guard attributes[.type] as? FileAttributeType == .typeRegular,
              let owner = attributes[.ownerAccountID] as? NSNumber,
              owner.uint32Value == getuid(),
              let permissions = attributes[.posixPermissions] as? NSNumber
        else {
            return false
        }
        return permissions.uint16Value & 0o077 == 0
    }

    private func removeStaleReadyFile(_ file: URL) throws {
        guard fileManager.fileExists(atPath: file.path) else { return }
        guard try isPrivateRegularFile(file) else { throw LocalNodeError.insecurePrivateFile }
        try fileManager.removeItem(at: file)
    }

    private func rotateLogIfNeeded(_ file: URL) throws {
        guard let attributes = try? fileManager.attributesOfItem(atPath: file.path),
              let size = attributes[.size] as? NSNumber,
              size.uint64Value > Self.maximumLogBytes
        else {
            return
        }
        guard try isPrivateRegularFile(file) else { throw LocalNodeError.insecurePrivateFile }
        try Data().write(to: file, options: .atomic)
        try fileManager.setAttributes([.posixPermissions: 0o600], ofItemAtPath: file.path)
    }

    private func readLogTail(_ file: URL) -> String {
        guard let data = try? Data(contentsOf: file), !data.isEmpty else {
            return "No service log was produced."
        }
        let suffix = data.suffix(4_096)
        return String(decoding: suffix, as: UTF8.self)
    }

    private func stopManagedNode() {
        guard !managedStopRequested else { return }
        if let process = ownedProcess {
            guard process.isRunning else {
                finishManagedNodeStop(expectedProcess: process, expectedProcessID: managedProcessID)
                return
            }
            managedStopRequested = true
            process.terminate()
        } else if let processID = managedProcessID,
                  processID > 0,
                  let readyFile = managedReadyFile,
                  (try? readReadyFile(readyFile).processId) == processID {
            managedStopRequested = true
            if Darwin.kill(processID, SIGTERM) != 0, errno == ESRCH {
                finishManagedNodeStop(expectedProcess: nil, expectedProcessID: processID)
                return
            }
        } else if managedProcessID == nil {
            finishManagedNodeStop(expectedProcess: nil, expectedProcessID: nil)
            return
        }
        try? logHandle?.close()
        logHandle = nil
    }

    private func stopManagedNodeAndWait() async throws {
        stopManagedNode()
        if let process = ownedProcess {
            let processID = managedProcessID
            let clock = ContinuousClock()
            let deadline = clock.now.advanced(by: Self.shutdownTimeout)
            while clock.now < deadline {
                guard process.isRunning else {
                    finishManagedNodeStop(
                        expectedProcess: process,
                        expectedProcessID: processID
                    )
                    return
                }
                try await Task.sleep(for: .milliseconds(50))
            }
            guard !process.isRunning else { throw LocalNodeError.shutdownTimedOut }
            finishManagedNodeStop(expectedProcess: process, expectedProcessID: processID)
            return
        }
        let processID = managedProcessID
        guard let processID, processID > 0 else { return }
        let clock = ContinuousClock()
        let deadline = clock.now.advanced(by: Self.shutdownTimeout)
        while clock.now < deadline {
            if Darwin.kill(processID, 0) != 0 {
                guard errno == ESRCH else { throw LocalNodeError.shutdownTimedOut }
                finishManagedNodeStop(expectedProcess: nil, expectedProcessID: processID)
                return
            }
            try await Task.sleep(for: .milliseconds(50))
        }
        if Darwin.kill(processID, 0) == 0 || errno != ESRCH {
            throw LocalNodeError.shutdownTimedOut
        }
        finishManagedNodeStop(expectedProcess: nil, expectedProcessID: processID)
    }

    private func finishManagedNodeStop(
        expectedProcess: Process?,
        expectedProcessID: Int32?
    ) {
        guard ownedProcess === expectedProcess,
              managedProcessID == expectedProcessID
        else { return }
        ownedProcess = nil
        managedProcessID = nil
        managedReadyFile = nil
        managedStopRequested = false
        managedAPIToken = nil
        try? logHandle?.close()
        logHandle = nil
    }
}

private struct ManagedPaths {
    let helper: URL
    let dataDirectory: URL
    let readyFile: URL
    let logFile: URL
}

private struct ManagedNodeReady: Decodable {
    let schemaVersion: UInt16
    let apiBaseUrl: String
    let peerAddress: String
    let processId: Int32
}

private enum LocalNodeError: LocalizedError {
    case helperMissing(String)
    case logUnavailable(String)
    case launchFailed(String)
    case exitedDuringStartup(String)
    case startupTimedOut(String)
    case shutdownTimedOut
    case invalidReadyFile
    case existingServiceUnavailable
    case insecurePrivateFile
    case recoveryTargetNotFresh
    case invalidRecoveryMaterial

    var errorDescription: String? {
        switch self {
        case let .helperMissing(path):
            "The bundled Covalent service is missing or not executable at \(path)."
        case let .logUnavailable(path):
            "The Covalent service log could not be opened at \(path)."
        case let .launchFailed(details):
            "The bundled Covalent service could not launch. \(details)"
        case let .exitedDuringStartup(details):
            "The bundled Covalent service exited during startup. \(details)"
        case let .startupTimedOut(details):
            "The bundled Covalent service did not become ready. \(details)"
        case .shutdownTimedOut:
            "The previous bundled Covalent service did not stop safely. Try again after it exits."
        case .invalidReadyFile:
            "The local Covalent service produced an invalid readiness record."
        case .existingServiceUnavailable:
            "The previous Covalent service is still running but could not be verified safely. Quit it before trying again."
        case .insecurePrivateFile:
            "A local Covalent service credential or readiness file has unsafe permissions."
        case .recoveryTargetNotFresh:
            "Covalent will not recover over an existing local identity. Choose a Mac without local Covalent data."
        case .invalidRecoveryMaterial:
            "The recovery kit or recovery code file must be an owner-only regular file."
        }
    }
}
