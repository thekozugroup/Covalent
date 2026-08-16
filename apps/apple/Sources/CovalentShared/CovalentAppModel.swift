import Combine
import Foundation

@MainActor
public protocol LocalNodeBootstrapping: AnyObject {
    func start() async throws -> NodeConnectionConfiguration
}

public enum ServicePhase: Equatable, Sendable {
    case starting
    case ready
    case needsAuthorization
    case offline
}

public enum AppSection: String, CaseIterable, Identifiable, Sendable {
    case overview
    case backups
    case devices
    case settings

    public var id: String { rawValue }

    public var label: String {
        switch self {
        case .overview: "Overview"
        case .backups: "Backups"
        case .devices: "Devices"
        case .settings: "Settings"
        }
    }

    public var systemImage: String {
        switch self {
        case .overview: "square.grid.2x2"
        case .backups: "externaldrive"
        case .devices: "laptopcomputer.and.iphone"
        case .settings: "gearshape"
        }
    }
}

public enum AppPresentation: String, Identifiable, Sendable {
    case connection
    case newBackup
    case pairDevice
    case importSettings

    public var id: String { rawValue }
}

public enum ActiveTaskKind: String, Sendable {
    case backup
    case verification
    case restore

    public var label: String {
        switch self {
        case .backup: "Backing up"
        case .verification: "Verifying"
        case .restore: "Restoring"
        }
    }
}

public struct ActiveTask: Equatable, Sendable {
    public let kind: ActiveTaskKind
    public let jobId: String?
    public let title: String
    public var state: JobState

    public init(kind: ActiveTaskKind, jobId: String?, title: String, state: JobState = .running) {
        self.kind = kind
        self.jobId = jobId
        self.title = title
        self.state = state
    }
}

public struct AppAlert: Identifiable, Equatable, Sendable {
    public let id = UUID()
    public let title: String
    public let message: String

    public init(title: String, message: String) {
        self.title = title
        self.message = message
    }
}

public struct RestorePreviewContext: Equatable, Sendable {
    public let plan: RestorePlan
    public let destinationGrantId: UUID
    public let destinationDisplayName: String
}

public struct RestoreSetupRequest: Equatable, Identifiable, Sendable {
    public let id: UUID
    public let snapshotId: UUID

    public init(id: UUID = UUID(), snapshotId: UUID) {
        self.id = id
        self.snapshotId = snapshotId
    }
}

@MainActor
public final class CovalentAppModel: ObservableObject {
    @Published public var selectedSection: AppSection = .overview
    @Published public var presentation: AppPresentation?
    @Published public private(set) var phase: ServicePhase = .starting
    @Published public private(set) var status: NodeStatus?
    @Published public private(set) var settings: ExportedDeviceSettings?
    @Published public private(set) var providers: [ProviderConnection] = []
    @Published public private(set) var backups: [BackupSummary] = []
    @Published public private(set) var discoveryCandidates: [DiscoveryCandidate] = []
    @Published public private(set) var directoryGrants: [SelectedDirectoryGrant] = []
    @Published public private(set) var snapshots: [SnapshotRecord] = []
    @Published public private(set) var activeTask: ActiveTask?
    @Published public var restoreSetupRequest: RestoreSetupRequest?
    @Published public private(set) var restorePreview: RestorePreviewContext?
    @Published public private(set) var lastRestoreResult: RestoreResponse?
    @Published public private(set) var lastRefreshedAt: Date?
    @Published public var alert: AppAlert?

    private let connectionStore: SecureNodeConnectionStore
    private let persistence: AppleAppPersistence
    private let localNodeBootstrapper: (any LocalNodeBootstrapping)?
    private var configuration: NodeConnectionConfiguration
    private var client: NodeClient
    private var didStart = false

    public init(
        connectionStore: SecureNodeConnectionStore = SecureNodeConnectionStore(),
        persistence: AppleAppPersistence = AppleAppPersistence(),
        client: NodeClient? = nil,
        configuration: NodeConnectionConfiguration? = nil,
        localNodeBootstrapper: (any LocalNodeBootstrapping)? = nil
    ) {
        self.connectionStore = connectionStore
        self.persistence = persistence
        self.localNodeBootstrapper = localNodeBootstrapper
        let loadedConfiguration: NodeConnectionConfiguration
        if let configuration {
            loadedConfiguration = configuration
        } else {
            #if DEBUG
            let environment = ProcessInfo.processInfo.environment
            if let address = environment["COVALENT_UI_TEST_BASE_URL"],
               let url = URL(string: address),
               let token = environment["COVALENT_UI_TEST_TOKEN"],
               let testingConfiguration = try? NodeConnectionConfiguration(baseURL: url, apiToken: token) {
                loadedConfiguration = testingConfiguration
            } else {
                loadedConfiguration = (try? connectionStore.load()) ?? .localDefault
            }
            #else
            loadedConfiguration = (try? connectionStore.load()) ?? .localDefault
            #endif
        }
        self.configuration = loadedConfiguration
        self.client = client ?? NodeClient(configuration: loadedConfiguration)
    }

    public var isAuthorized: Bool { configuration.apiToken != nil && phase == .ready }
    public var sourceGrants: [SelectedDirectoryGrant] {
        directoryGrants.filter { $0.purpose == .backupSource }
    }
    public var restoreGrants: [SelectedDirectoryGrant] {
        directoryGrants.filter { $0.purpose == .restoreDestination }
    }
    public var rememberedBackups: [RememberedBackup] { settings?.rememberedBackups ?? [] }

    public var serviceStatusLabel: String {
        switch phase {
        case .starting: "Connecting"
        case .ready: "Ready"
        case .needsAuthorization: "Setup required"
        case .offline: "Offline"
        }
    }

    public func requestNewBackup() {
        selectedSection = .backups
        presentation = .newBackup
    }

    public func requestRestoreLatest() {
        guard let snapshot = snapshots.first else {
            selectedSection = .backups
            alert = AppAlert(
                title: "No backup to restore",
                message: "Create a backup before starting a restore."
            )
            return
        }
        selectedSection = .backups
        restoreSetupRequest = RestoreSetupRequest(snapshotId: snapshot.id)
    }

    public func start() async {
        guard !didStart else { return }
        didStart = true
        do {
            async let grants = persistence.loadDirectoryGrants()
            async let history = persistence.loadSnapshots()
            directoryGrants = try await grants
            snapshots = try await history.sorted { $0.createdAt > $1.createdAt }
        } catch {
            report(error, title: "Saved access could not be loaded")
        }
        await refresh()
    }

    public func refresh() async {
        phase = .starting
        do {
            if let localNodeBootstrapper {
                let managedConfiguration = try await localNodeBootstrapper.start()
                if managedConfiguration != configuration {
                    configuration = managedConfiguration
                    client = NodeClient(configuration: managedConfiguration)
                }
            }
            let nodeStatus = try await client.status()
            status = nodeStatus
            guard configuration.apiToken != nil else {
                settings = nil
                providers = []
                backups = []
                discoveryCandidates = []
                phase = .needsAuthorization
                return
            }
            async let exportedSettings = client.exportSettings()
            async let providerConnections = client.providers()
            async let backupSummaries = client.backups()
            settings = try await exportedSettings
            providers = try await providerConnections
            backups = try await backupSummaries
            discoveryCandidates = (try? await client.discoveryCandidates()) ?? []
            lastRefreshedAt = Date()
            phase = .ready
        } catch NodeClientError.missingToken {
            phase = .needsAuthorization
        } catch NodeClientError.unauthorized {
            phase = .needsAuthorization
            report(NodeClientError.unauthorized, title: "Reconnect this app")
        } catch {
            phase = .offline
            report(
                error,
                title: localNodeBootstrapper == nil
                    ? "Local service unavailable"
                    : "Local service could not start"
            )
        }
    }

    public func connect(
        serviceAddress: String,
        token: String,
        deviceName: String,
        lanDiscoveryEnabled: Bool
    ) async -> Bool {
        do {
            guard let url = URL(string: serviceAddress.trimmingCharacters(in: .whitespacesAndNewlines)) else {
                throw NodeClientError.invalidServiceURL
            }
            let candidateConfiguration = try NodeConnectionConfiguration(baseURL: url, apiToken: token)
            let candidateClient = NodeClient(configuration: candidateConfiguration)
            let nodeStatus = try await candidateClient.status()
            var exported = try await candidateClient.exportSettings()
            let cleanName = try validatedDeviceName(deviceName.isEmpty ? nodeStatus.deviceName : deviceName)
            if cleanName != exported.deviceName || lanDiscoveryEnabled != exported.lanDiscoveryEnabled {
                exported = ExportedDeviceSettings(
                    schemaVersion: exported.schemaVersion,
                    deviceName: cleanName,
                    lanDiscoveryEnabled: lanDiscoveryEnabled,
                    rememberedBackups: exported.rememberedBackups
                )
                try await candidateClient.importSettings(exported, confirmed: true)
            }
            try connectionStore.save(candidateConfiguration)
            configuration = candidateConfiguration
            client = candidateClient
            presentation = nil
            await refresh()
            return phase == .ready
        } catch {
            report(error, title: "Connection failed")
            return false
        }
    }

    public func disconnect() async {
        do {
            try connectionStore.clear()
        } catch {
            report(error, title: "Connection could not be cleared")
        }
        configuration = .localDefault
        client = NodeClient(configuration: configuration)
        status = nil
        settings = nil
        providers = []
        backups = []
        discoveryCandidates = []
        phase = .needsAuthorization
        presentation = .connection
    }

    public func currentConnectionAddress() -> String {
        configuration.baseURL.absoluteString
    }

    public func addDirectoryGrant(url: URL, purpose: DirectoryAccessPurpose) async -> SelectedDirectoryGrant? {
        do {
            let grant = try SelectedDirectoryGrant.capture(url: url, purpose: purpose)
            directoryGrants.removeAll { existing in
                existing.purpose == purpose && existing.displayName == grant.displayName
            }
            directoryGrants.append(grant)
            try await persistence.saveDirectoryGrants(directoryGrants)
            return grant
        } catch {
            report(error, title: "Folder access was not saved")
            return nil
        }
    }

    public func removeDirectoryGrant(id: UUID) async {
        directoryGrants.removeAll { $0.id == id }
        do {
            try await persistence.saveDirectoryGrants(directoryGrants)
        } catch {
            report(error, title: "Folder access could not be removed")
        }
    }

    public func createBackup(
        displayName: String,
        existingBackupId: UUID?,
        sourceGrantId: UUID,
        selectedProviderIds: Set<UUID>
    ) async -> SnapshotRecord? {
        do {
            guard activeTask == nil else { throw AppModelError.operationInProgress }
            guard let grant = directoryGrants.first(where: { $0.id == sourceGrantId && $0.purpose == .backupSource }) else {
                throw AppModelError.folderPermissionMissing
            }
            let name = try validatedBackupName(displayName)
            let connectedIds = Set(providers.map(\.peerId))
            guard selectedProviderIds.isSubset(of: connectedIds) else {
                throw AppModelError.providerNotConnected
            }
            let jobId = "backup-\(UUID().uuidString.lowercased())"
            let snapshotId = Self.snapshotIdentifier()
            activeTask = ActiveTask(kind: .backup, jobId: jobId, title: name)
            defer { activeTask = nil }
            let resolved = try grant.resolve()
            let client = self.client
            let response = try await resolved.withCoordinatedRead { sourceURL in
                try await client.createBackupArchive(
                    sourceURL: sourceURL,
                    metadata: ArchiveBackupMetadata(
                        backupId: existingBackupId,
                        displayName: name,
                        snapshotId: snapshotId,
                        jobId: jobId,
                        selectedProviderIds: selectedProviderIds.sorted { $0.uuidString < $1.uuidString }
                    )
                )
            }
            let record = SnapshotRecord(
                backupId: response.backupId,
                displayName: name,
                snapshotId: response.snapshotId,
                sourceGrantId: grant.id,
                selectedProviderIds: selectedProviderIds.sorted { $0.uuidString < $1.uuidString },
                response: response,
                integrity: response.degradedFailures == 0 ? .unknown : .degraded
            )
            snapshots.insert(record, at: 0)
            try await persistence.saveSnapshots(snapshots)
            settings = try await client.exportSettings()
            backups = try await client.backups()
            selectedSection = .backups
            presentation = nil
            return record
        } catch {
            report(error, title: "Backup did not finish")
            return nil
        }
    }

    public func controlActiveTask(_ action: JobAction) async {
        do {
            guard let activeTask, let jobId = activeTask.jobId else {
                throw AppModelError.noControllableOperation
            }
            let response = try await client.controlJob(jobId: jobId, action: action)
            self.activeTask?.state = response.state
        } catch {
            report(error, title: "Job control failed")
        }
    }

    public func verify(_ record: SnapshotRecord, repair: Bool = false) async -> VerifyResponse? {
        do {
            guard activeTask == nil else { throw AppModelError.operationInProgress }
            updateIntegrity(recordId: record.id, to: .checking)
            activeTask = ActiveTask(kind: .verification, jobId: nil, title: record.displayName)
            defer { activeTask = nil }
            let result = try await client.verifySnapshot(
                SnapshotRequest(
                    backupId: record.backupId,
                    snapshotId: record.snapshotId,
                    verifyProviders: !record.selectedProviderIds.isEmpty,
                    repair: repair
                )
            )
            let availabilityNeedsAttention = result.providerAvailability.values.contains { $0 != .complete }
            let integrity: SnapshotIntegrity = result.intact
                ? (availabilityNeedsAttention ? .degraded : .intact)
                : .corrupt
            updateIntegrity(recordId: record.id, to: integrity)
            try await persistence.saveSnapshots(snapshots)
            return result
        } catch {
            updateIntegrity(recordId: record.id, to: .unknown)
            report(error, title: "Verification failed")
            return nil
        }
    }

    public func previewRestore(
        record: SnapshotRecord,
        destinationGrantId: UUID,
        conflictPolicy: ConflictPolicy
    ) async -> RestorePlan? {
        do {
            guard activeTask == nil else { throw AppModelError.operationInProgress }
            guard let grant = directoryGrants.first(where: {
                $0.id == destinationGrantId && $0.purpose == .restoreDestination
            }) else {
                throw AppModelError.folderPermissionMissing
            }
            let jobId = "restore-\(UUID().uuidString.lowercased())"
            let resolved = try grant.resolve()
            let client = self.client
            let plan = try await resolved.withCoordinatedWrite { targetURL in
                try await Task.detached(priority: .userInitiated) {
                    try AppleArchiveTransfer.requireEmptyDirectory(targetURL)
                }.value
                return try await client.previewArchiveRestore(
                        backupId: record.backupId,
                        snapshotId: record.snapshotId,
                        conflictPolicy: conflictPolicy,
                        jobId: jobId
                )
            }
            restorePreview = RestorePreviewContext(
                plan: plan,
                destinationGrantId: destinationGrantId,
                destinationDisplayName: grant.displayName
            )
            return plan
        } catch {
            report(error, title: "Restore preview failed")
            return nil
        }
    }

    public func executeRestore() async -> RestoreResponse? {
        do {
            guard activeTask == nil else { throw AppModelError.operationInProgress }
            guard let context = restorePreview,
                  let grant = directoryGrants.first(where: { $0.id == context.destinationGrantId })
            else {
                throw AppModelError.restorePreviewMissing
            }
            activeTask = ActiveTask(
                kind: .restore,
                jobId: context.plan.jobId,
                title: snapshots.first(where: { $0.snapshotId == context.plan.snapshotId })?.displayName ?? "Backup"
            )
            defer { activeTask = nil }
            let resolved = try grant.resolve()
            let client = self.client
            let response = try await resolved.withCoordinatedWrite { targetURL in
                try await client.executeArchiveRestore(context.plan, targetURL: targetURL)
            }
            lastRestoreResult = response
            restorePreview = nil
            return response
        } catch {
            report(error, title: "Restore did not finish")
            return nil
        }
    }

    public func dismissRestorePreview() {
        restorePreview = nil
    }

    public func clearRestoreResult() {
        lastRestoreResult = nil
    }

    public func updateSettings(deviceName: String, lanDiscoveryEnabled: Bool) async -> Bool {
        do {
            guard let current = settings else { throw AppModelError.notConnected }
            let updated = ExportedDeviceSettings(
                schemaVersion: current.schemaVersion,
                deviceName: try validatedDeviceName(deviceName),
                lanDiscoveryEnabled: lanDiscoveryEnabled,
                rememberedBackups: current.rememberedBackups
            )
            try await client.importSettings(updated, confirmed: true)
            await refresh()
            return phase == .ready
        } catch {
            report(error, title: "Settings were not saved")
            return false
        }
    }

    public func exportSettingsData() async -> Data? {
        do {
            let exported = try await client.exportSettings()
            let encoder = JSONEncoder()
            encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
            return try encoder.encode(exported)
        } catch {
            report(error, title: "Settings export failed")
            return nil
        }
    }

    public func importSettingsData(_ data: Data) async -> Bool {
        do {
            guard data.count <= 2 * 1_024 * 1_024 else { throw AppModelError.settingsFileTooLarge }
            let imported = try JSONDecoder().decode(ExportedDeviceSettings.self, from: data)
            guard imported.schemaVersion == 1 else { throw AppModelError.unsupportedSettings }
            _ = try validatedDeviceName(imported.deviceName)
            try await client.importSettings(imported, confirmed: true)
            await refresh()
            return phase == .ready
        } catch {
            report(error, title: "Settings import failed")
            return false
        }
    }

    public func refreshDiscovery() async {
        do {
            discoveryCandidates = try await client.discoveryCandidates()
        } catch {
            report(error, title: "Nearby devices could not be refreshed")
        }
    }

    public func defaultInvitationEndpoint() async -> String? {
        do {
            let identity = try await client.transportIdentity()
            return "127.0.0.1:\(identity.peerPort)"
        } catch {
            report(error, title: "Transport identity unavailable")
            return nil
        }
    }

    public func createInvitation(endpoint: String) async -> PairingInvitation? {
        do {
            let cleanEndpoint = endpoint.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !cleanEndpoint.isEmpty, cleanEndpoint.utf8.count <= 512 else {
                throw AppModelError.invalidEndpoint
            }
            return try await client.createPairingInvitation(
                lifetimeMilliseconds: 10 * 60 * 1_000,
                endpoints: [cleanEndpoint]
            )
        } catch {
            report(error, title: "Invitation could not be created")
            return nil
        }
    }

    public func acceptInvitation(
        json: String,
        responderRoles: Set<PeerRole>,
        inviterRoles: Set<PeerRole>
    ) async -> PairingSession? {
        do {
            guard let settings else { throw AppModelError.notConnected }
            let invitation = try decode(PairingInvitation.self, from: json)
            return try await client.acceptPairingInvitation(
                invitation,
                responderName: settings.deviceName,
                responderRoles: responderRoles,
                inviterRoles: inviterRoles
            )
        } catch {
            report(error, title: "Invitation could not be accepted")
            return nil
        }
    }

    public func confirmPairing(
        sessionJSON: String,
        asInviter: Bool
    ) async -> PairingSession? {
        do {
            let session = try decode(PairingSession.self, from: sessionJSON)
            if asInviter {
                return try await client.confirmPairingAsInviter(
                    session,
                    displayedCode: session.authenticationString
                )
            }
            return try await client.confirmPairingAsResponder(
                session,
                displayedCode: session.authenticationString
            )
        } catch {
            report(error, title: "Pairing confirmation failed")
            return nil
        }
    }

    public func finalizePairing(sessionJSON: String, asInviter: Bool) async -> PairingConfirmation? {
        do {
            let session = try decode(PairingSession.self, from: sessionJSON)
            guard session.isMutuallySigned else { throw AppModelError.pairingNotMutuallyConfirmed }
            let confirmation = asInviter
                ? try await client.finalizePairingAsInviter(session)
                : try await client.finalizePairingAsResponder(session)
            await refresh()
            return confirmation
        } catch {
            report(error, title: "Pairing could not be finalized")
            return nil
        }
    }

    public func connectProvider(peerId: UUID, address: String, certificateDer: String) async -> Bool {
        do {
            _ = try await client.connectProvider(
                peerId: peerId,
                address: address.trimmingCharacters(in: .whitespacesAndNewlines),
                certificateDer: certificateDer.trimmingCharacters(in: .whitespacesAndNewlines)
            )
            providers = try await client.providers()
            return true
        } catch {
            report(error, title: "Provider connection failed")
            return false
        }
    }

    public func disconnectProvider(_ provider: ProviderConnection) async {
        do {
            try await client.disconnectProvider(peerId: provider.peerId)
            providers = try await client.providers()
        } catch {
            report(error, title: "Device could not be disconnected")
        }
    }

    public func revokeProvider(_ provider: ProviderConnection) async {
        do {
            try await client.revokePeer(peerId: provider.peerId)
            providers = try await client.providers()
        } catch {
            report(error, title: "Device access could not be revoked")
        }
    }

    public func transferJSON<Value: Encodable>(_ value: Value) throws -> String {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
        return String(decoding: try encoder.encode(value), as: UTF8.self)
    }

    public func pairingSession(from json: String) throws -> PairingSession {
        try decode(PairingSession.self, from: json)
    }

    public func pairingInvitation(from json: String) throws -> PairingInvitation {
        try decode(PairingInvitation.self, from: json)
    }

    public func clearAlert() {
        alert = nil
    }

    private func decode<Value: Decodable>(_ type: Value.Type, from json: String) throws -> Value {
        guard let data = json.data(using: .utf8), data.count <= 2 * 1_024 * 1_024 else {
            throw AppModelError.transferTooLarge
        }
        return try JSONDecoder().decode(type, from: data)
    }

    private func updateIntegrity(recordId: UUID, to integrity: SnapshotIntegrity) {
        guard let index = snapshots.firstIndex(where: { $0.id == recordId }) else { return }
        snapshots[index].integrity = integrity
    }

    private func validatedDeviceName(_ value: String) throws -> String {
        let clean = value.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !clean.isEmpty, clean.count <= 80 else { throw AppModelError.invalidDeviceName }
        return clean
    }

    private func validatedBackupName(_ value: String) throws -> String {
        let clean = value.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !clean.isEmpty, clean.count <= 120 else { throw AppModelError.invalidBackupName }
        return clean
    }

    private func report(_ error: Error, title: String) {
        let message = (error as? LocalizedError)?.errorDescription ?? String(describing: error)
        alert = AppAlert(title: title, message: message)
    }

    private static func snapshotIdentifier() -> String {
        let milliseconds = UInt64(Date().timeIntervalSince1970 * 1_000)
        return "s\(milliseconds)-\(UUID().uuidString.lowercased())"
    }
}

public enum AppModelError: Error, Equatable, Sendable {
    case notConnected
    case operationInProgress
    case noControllableOperation
    case folderPermissionMissing
    case providerNotConnected
    case restorePreviewMissing
    case invalidDeviceName
    case invalidBackupName
    case invalidEndpoint
    case unsupportedSettings
    case settingsFileTooLarge
    case transferTooLarge
    case pairingNotMutuallyConfirmed
}

extension AppModelError: LocalizedError {
    public var errorDescription: String? {
        switch self {
        case .notConnected: "Connect to the local Covalent service first."
        case .operationInProgress: "Wait for the current backup, verification, or restore to finish."
        case .noControllableOperation: "The current operation cannot be paused or cancelled."
        case .folderPermissionMissing: "Choose the folder again to restore access."
        case .providerNotConnected: "One of the selected replica devices is no longer connected."
        case .restorePreviewMissing: "Create a fresh restore preview before restoring files."
        case .invalidDeviceName: "Use a device name from 1 to 80 characters."
        case .invalidBackupName: "Use a backup name from 1 to 120 characters."
        case .invalidEndpoint: "Enter a reachable host and port for this device."
        case .unsupportedSettings: "This settings file uses an unsupported schema version."
        case .settingsFileTooLarge: "The settings file is larger than the 2 MiB limit."
        case .transferTooLarge: "The pairing transfer is larger than the 2 MiB limit."
        case .pairingNotMutuallyConfirmed: "Both devices must sign the matching code before pairing can finish."
        }
    }
}
