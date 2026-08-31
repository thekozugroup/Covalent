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
    /// Exists only while this app process owns or has adopted the local node.
    /// It is never written to the node data directory or launch metadata.
    private var managedAPIToken: String?
    private var logHandle: FileHandle?
    private var terminationObserver: NSObjectProtocol?

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

    func start() async throws -> NodeConnectionConfiguration {
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

    private func reconnectToExistingConfiguration(paths: ManagedPaths) async throws -> NodeConnectionConfiguration? {
        guard let ready = try? readReadyFile(paths.readyFile),
              ready.processId > 0,
              Darwin.kill(ready.processId, 0) == 0
        else {
            return nil
        }
        managedProcessID = ready.processId
        managedReadyFile = paths.readyFile
        let clock = ContinuousClock()
        let deadline = clock.now.advanced(by: Self.existingHealthTimeout)
        while clock.now < deadline {
            if let configuration = try await healthyExistingConfiguration(paths: paths) {
                return configuration
            }
            guard Darwin.kill(ready.processId, 0) == 0 else { return nil }
            try await Task.sleep(for: .milliseconds(100))
        }
        return nil
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

    private func managedPaths() throws -> ManagedPaths {
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
            dataDirectory = try defaultDataDirectory()
        }
        #else
        dataDirectory = try defaultDataDirectory()
        #endif
        return ManagedPaths(
            helper: executableDirectory.appending(path: "covalent-node"),
            dataDirectory: dataDirectory,
            readyFile: dataDirectory.appending(path: "node-ready.json"),
            logFile: dataDirectory.appending(path: "node.log")
        )
    }

    private func defaultDataDirectory() throws -> URL {
        let applicationSupport = try fileManager.url(
            for: .applicationSupportDirectory,
            in: .userDomainMask,
            appropriateFor: nil,
            create: true
        )
        return applicationSupport
            .appending(path: "Covalent", directoryHint: .isDirectory)
            .appending(path: "Node", directoryHint: .isDirectory)
    }

    private func preparePrivateDirectory(_ directory: URL) throws {
        try fileManager.createDirectory(at: directory, withIntermediateDirectories: true)
        try fileManager.setAttributes([.posixPermissions: 0o700], ofItemAtPath: directory.path)
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
        let processID = managedProcessID
        let readyFile = managedReadyFile
        managedProcessID = nil
        managedReadyFile = nil
        managedAPIToken = nil
        let process = ownedProcess
        ownedProcess = nil
        if let process, process.isRunning {
            process.terminate()
        } else if let processID,
                  processID > 0,
                  let readyFile,
                  (try? readReadyFile(readyFile).processId) == processID {
            _ = Darwin.kill(processID, SIGTERM)
        }
        try? logHandle?.close()
        logHandle = nil
    }

    private func stopManagedNodeAndWait() async throws {
        let processID = managedProcessID
        stopManagedNode()
        guard let processID, processID > 0 else { return }
        let clock = ContinuousClock()
        let deadline = clock.now.advanced(by: Self.shutdownTimeout)
        while clock.now < deadline {
            guard Darwin.kill(processID, 0) == 0 else { return }
            try await Task.sleep(for: .milliseconds(50))
        }
        guard Darwin.kill(processID, 0) != 0 else {
            throw LocalNodeError.shutdownTimedOut
        }
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
    case insecurePrivateFile

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
        case .insecurePrivateFile:
            "A local Covalent service credential or readiness file has unsafe permissions."
        }
    }
}
