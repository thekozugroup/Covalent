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
    case networkPairing
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
    /// Live byte counts, once the transfer has any to report. `nil` before the
    /// first callback and for work that moves no bytes (verification).
    public var progress: TransferProgressSnapshot?

    public init(
        kind: ActiveTaskKind,
        jobId: String?,
        title: String,
        state: JobState = .running,
        progress: TransferProgressSnapshot? = nil
    ) {
        self.kind = kind
        self.jobId = jobId
        self.title = title
        self.state = state
        self.progress = progress
    }

    /// What to show beneath the title: real byte counts when the transfer has
    /// them, and the durable-checkpoint reassurance otherwise.
    public func statusDetail(pausedText: String, checkpointText: String) -> String {
        guard state != .paused else { return pausedText }
        guard let progress else { return checkpointText }
        switch progress.phase {
        case .preparing: return "Preparing…"
        case .finishing: return "Finishing up…"
        case .transferring: return progress.byteSummary
        }
    }
}

public struct AppAlert: Identifiable, Equatable, Sendable {
    public let id = UUID()
    public let title: String
    /// Plain-English lead. Never a raw error string.
    public let message: String
    /// The technical text behind `message`, shown only on request.
    public let detail: String?
    /// The way out of this failure, rendered as a real button beside "OK".
    public let recovery: RecoveryHint

    public init(
        title: String,
        message: String,
        detail: String? = nil,
        recovery: RecoveryHint = .none
    ) {
        self.title = title
        self.message = message
        self.detail = detail
        self.recovery = recovery
    }

    /// The button label for ``recovery``, or `nil` when the only honest
    /// option is to acknowledge. An alert offering only "OK" on a recoverable
    /// failure is a bug, so every actionable hint must return a title here.
    public var recoveryActionTitle: String? {
        switch recovery {
        case .none, .freeUpSpace: nil
        case .retry: "Try Again"
        case .reconnect: "Reconnect"
        case .checkNetworkSettings: "Open Settings"
        case .chooseAnotherDevice: "Choose Another Device"
        case .chooseFolderAgain: "Choose Folder"
        case .previewRestoreAgain: "Preview Again"
        }
    }

    /// `true` when the recovery must be performed by the platform layer
    /// (opening system Settings) rather than by the model.
    public var recoveryOpensSystemSettings: Bool { recovery == .checkNetworkSettings }
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
    @Published public var backupDraftBackupId: UUID?
    @Published public private(set) var networkPairings: [NetworkPairing] = []
    @Published public private(set) var activeNetworkPairing: NetworkPairing?
    @Published public private(set) var startingPairingCandidateID: String?
    @Published public private(set) var startingPairingAddress: String?
    @Published public private(set) var directoryGrants: [SelectedDirectoryGrant] = []
    @Published public private(set) var snapshots: [SnapshotRecord] = []
    @Published public private(set) var activeTask: ActiveTask?
    @Published public var restoreSetupRequest: RestoreSetupRequest?
    @Published public private(set) var restorePreview: RestorePreviewContext?
    @Published public private(set) var lastRestoreResult: RestoreResponse?
    @Published public private(set) var lastRefreshedAt: Date?
    @Published public var alert: AppAlert?

    /// The operation "Try Again" re-runs. Held outside ``AppAlert`` so the
    /// alert itself stays `Equatable` and `Sendable`.
    private var alertRetry: (@MainActor () async -> Void)?

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

    public func requestNewBackup(existingBackupId: UUID? = nil) {
        selectedSection = .backups
        backupDraftBackupId = existingBackupId
        presentation = .newBackup
    }

    public func requestManualPairing() {
        selectedSection = .devices
        presentation = .pairDevice
    }

    public func startNetworkPairing(candidate: DiscoveryCandidate) async {
        guard candidate.isCompatible else {
            alert = AppAlert(
                title: "This device can't pair",
                message: "That device runs a version of Covalent that can't pair with this one. "
                    + "Update it, or pick a different device.",
                recovery: .chooseAnotherDevice
            )
            return
        }
        startingPairingCandidateID = candidate.id
        defer { startingPairingCandidateID = nil }
        await startNetworkPairing(candidateAddress: candidate.endpoint)
    }

    public func startNetworkPairing(candidateAddress: String) async {
        let address = candidateAddress.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !address.isEmpty, address.utf8.count <= 253, !address.contains(where: \Character.isWhitespace) else {
            alert = AppAlert(
                title: "That address doesn't look right",
                message: "Enter the other device's name or IP address, with no spaces. "
                    + "Include its Covalent port if it uses a non-standard one."
            )
            return
        }
        startingPairingAddress = address
        defer { startingPairingAddress = nil }
        do {
            let pairing = try await client.startNetworkPairing(candidateAddress: address)
            upsertNetworkPairing(pairing)
            activeNetworkPairing = pairing
            presentation = .networkPairing
        } catch let error as NodeClientError where Self.isUnsupportedRoute(error) {
            // A node that predates quick pairing answers 404 rather than
            // failing the request, so say what is actually wrong instead of
            // surfacing a bare "not found".
            alert = AppAlert(
                title: "Quick pairing isn't available",
                message: "Your backup server is running an older version that doesn't support quick pairing. "
                    + "Update it, or pair using Advanced recovery.",
                detail: ErrorPresenter.detail(for: error),
                recovery: .chooseAnotherDevice
            )
        } catch {
            report(error, title: "Secure pairing couldn't start") { [weak self] in
                await self?.startNetworkPairing(candidateAddress: address)
            }
        }
    }

    private static func isUnsupportedRoute(_ error: NodeClientError) -> Bool {
        guard case let .api(status, code, _, _) = error else { return false }
        return status == 404 && (code == "route_not_found" || code == "http_404")
    }

    public func refreshNetworkPairings(reportErrors: Bool = false) async {
        guard isAuthorized else { return }
        do {
            let pending = try await client.pendingNetworkPairings()
            networkPairings = pending.sorted { $0.expiresAtUnixMs < $1.expiresAtUnixMs }
            if let activeNetworkPairing,
               let updated = pending.first(where: { $0.id == activeNetworkPairing.id }) {
                self.activeNetworkPairing = updated
                if updated.state == .complete {
                    try await establishProviderConnection(for: updated)
                }
            } else if presentation == nil,
                      activeTask == nil,
                      let incoming = pending.first(where: {
                          $0.direction == .incoming && $0.state != .failed
                      }) {
                activeNetworkPairing = incoming
                presentation = .networkPairing
            }
        } catch where !reportErrors {
            // Status refresh remains authoritative; transient pairing polling errors are retried.
        } catch {
            self.report(error, title: "Pairing requests couldn't be refreshed") { [weak self] in
                await self?.refreshNetworkPairings(reportErrors: true)
            }
        }
    }

    public func pollNetworkPairings() async {
        while !Task.isCancelled {
            await refreshNetworkPairings()
            do {
                try await Task.sleep(for: .seconds(2))
            } catch {
                return
            }
        }
    }

    public func confirmNetworkPairing(_ pairing: NetworkPairing) async {
        do {
            let updated = try await client.confirmNetworkPairing(
                pairingId: pairing.id,
                displayedCode: pairing.authenticationString
            )
            upsertNetworkPairing(updated)
            activeNetworkPairing = updated
            if updated.state == .complete {
                try await establishProviderConnection(for: updated)
            }
        } catch {
            report(error, title: "Pairing couldn't be confirmed") { [weak self] in
                await self?.confirmNetworkPairing(pairing)
            }
        }
    }

    public func dismissNetworkPairing(_ pairing: NetworkPairing) async {
        do {
            try await client.cancelNetworkPairing(pairingId: pairing.id)
        } catch {
            report(error, title: pairing.state == .complete ? "Pairing could not be acknowledged" : "Pairing could not be cancelled")
            return
        }
        networkPairings.removeAll { $0.id == pairing.id }
        if activeNetworkPairing?.id == pairing.id {
            activeNetworkPairing = nil
        }
        presentation = nil
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
                    ? "Can't reach your backup server"
                    : "Your backup server didn't start"
            ) { [weak self] in
                await self?.refresh()
            }
        }
    }

    public func connect(
        serviceAddress: String,
        token: String,
        trustedCertificateDER: Data? = nil,
        deviceName: String,
        lanDiscoveryEnabled: Bool
    ) async -> Bool {
        do {
            guard let url = URL(string: serviceAddress.trimmingCharacters(in: .whitespacesAndNewlines)) else {
                throw NodeClientError.invalidServiceURL
            }
            let candidateConfiguration = try NodeConnectionConfiguration(
                baseURL: url,
                apiToken: token,
                trustedCertificateDER: trustedCertificateDER
            )
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
            guard selectedProviderIds.isEmpty else {
                throw AppModelError.providerCapacityUnverified
            }
            let jobId = "backup-\(UUID().uuidString.lowercased())"
            let snapshotId = Self.snapshotIdentifier()
            activeTask = ActiveTask(kind: .backup, jobId: jobId, title: name)
            defer { activeTask = nil }
            let resolved = try grant.resolve()
            let client = self.client
            let onProgress = progressSink()
            let response = try await resolved.withCoordinatedRead { sourceURL in
                try await client.createBackupArchive(
                    sourceURL: sourceURL,
                    metadata: ArchiveBackupMetadata(
                        backupId: existingBackupId,
                        displayName: name,
                        snapshotId: snapshotId,
                        jobId: jobId,
                        selectedProviderIds: selectedProviderIds.sorted { $0.uuidString < $1.uuidString }
                    ),
                    onProgress: onProgress
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
            selectedSection = .backups
            presentation = nil
            do {
                try await client.acknowledgeJob(jobId: jobId)
            } catch {
                report(error, title: "Backup finished; staged retry cleanup is pending")
            }
            do {
                settings = try await client.exportSettings()
                backups = try await client.backups()
            } catch {
                report(error, title: "Backup finished; summary refresh is pending")
            }
            return record
        } catch {
            report(error, title: "Backup didn't finish") { [weak self] in
                _ = await self?.createBackup(
                    displayName: displayName,
                    existingBackupId: existingBackupId,
                    sourceGrantId: sourceGrantId,
                    selectedProviderIds: selectedProviderIds
                )
            }
            return nil
        }
    }

    /// A `URLSession`-safe sink that funnels transfer byte counts back onto
    /// the main actor and into ``activeTask``.
    ///
    /// `CovalentAppModel` is `@MainActor`-isolated and therefore implicitly
    /// `Sendable`, so the escaping closure can hold a weak reference safely.
    ///
    /// Callbacks arrive on a background queue and, for a multi-gigabyte
    /// transfer, arrive very often. Spawning a `Task` per callback would queue
    /// tens of thousands of main-actor jobs that all write the same property,
    /// so the sink coalesces: it keeps only the newest snapshot and allows a
    /// single hop to be in flight at a time. The UI still lands on the final
    /// value because whichever hop runs last reads the latest snapshot.
    private func progressSink() -> @Sendable (TransferProgressSnapshot) -> Void {
        let coalescer = ProgressCoalescer()
        return { [weak self] snapshot in
            guard coalescer.offer(snapshot) else { return }
            Task { @MainActor in
                guard let latest = coalescer.take() else { return }
                self?.activeTask?.progress = latest
            }
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
            report(error, title: "That didn't take effect") { [weak self] in
                await self?.controlActiveTask(action)
            }
        }
    }

    public func pauseActiveTaskForBackgroundExpiration() async {
        guard let jobId = activeTask?.jobId else { return }
        do {
            let response = try await client.controlJob(jobId: jobId, action: .pause)
            activeTask?.state = response.state
        } catch {
            report(error, title: "Transfer paused locally; reconnect to resume")
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
            report(error, title: "Verification didn't finish") { [weak self] in
                _ = await self?.verify(record, repair: repair)
            }
            return nil
        }
    }

    public func previewRestore(
        record: SnapshotRecord,
        destinationGrantId: UUID,
        conflictPolicy: ConflictPolicy
    ) async -> RestorePlan? {
        let jobId = "restore-\(UUID().uuidString.lowercased())"
        do {
            guard activeTask == nil else { throw AppModelError.operationInProgress }
            guard let grant = directoryGrants.first(where: {
                $0.id == destinationGrantId && $0.purpose == .restoreDestination
            }) else {
                throw AppModelError.folderPermissionMissing
            }
            let resolved = try grant.resolve()
            let client = self.client
            let plan = try await resolved.withCoordinatedWrite { targetURL in
                return try await client.previewArchiveRestore(
                        backupId: record.backupId,
                        snapshotId: record.snapshotId,
                        conflictPolicy: conflictPolicy,
                        jobId: jobId,
                        targetURL: targetURL
                )
            }
            let previousPreview = restorePreview
            restorePreview = RestorePreviewContext(
                plan: plan,
                destinationGrantId: destinationGrantId,
                destinationDisplayName: grant.displayName
            )
            if let previousPreview {
                try? await client.discardJob(jobId: previousPreview.plan.jobId)
            }
            return plan
        } catch {
            try? await client.discardJob(jobId: jobId)
            report(error, title: "Restore preview didn't finish") { [weak self] in
                _ = await self?.previewRestore(
                    record: record,
                    destinationGrantId: destinationGrantId,
                    conflictPolicy: conflictPolicy
                )
            }
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
            let onProgress = progressSink()
            let response = try await resolved.withCoordinatedWrite { targetURL in
                try await client.executeArchiveRestore(
                    context.plan,
                    targetURL: targetURL,
                    onProgress: onProgress
                )
            }
            lastRestoreResult = response
            restorePreview = nil
            do {
                try await client.acknowledgeJob(jobId: context.plan.jobId)
            } catch {
                report(error, title: "Restore finished; staged retry cleanup is pending")
            }
            return response
        } catch {
            report(error, title: "Restore didn't finish") { [weak self] in
                _ = await self?.executeRestore()
            }
            return nil
        }
    }

    public func dismissRestorePreview() {
        let jobId = restorePreview?.plan.jobId
        restorePreview = nil
        if let jobId {
            Task { try? await client.discardJob(jobId: jobId) }
        }
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
            report(error, title: "Nearby devices couldn't be refreshed") { [weak self] in
                await self?.refreshDiscovery()
            }
        }
    }

    private func upsertNetworkPairing(_ pairing: NetworkPairing) {
        networkPairings.removeAll { $0.id == pairing.id }
        networkPairings.append(pairing)
        networkPairings.sort { $0.expiresAtUnixMs < $1.expiresAtUnixMs }
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

    public func createInvitation() async -> PairingInvitation? {
        do {
            return try await client.createPairingInvitation(
                lifetimeMilliseconds: 10 * 60 * 1_000,
                endpoints: []
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

    public func connectProvider(using transport: PeerTransport) async -> Bool {
        do {
            let connected = try await client.connectProvider(using: transport)
            try Self.validateProviderBinding(connected, transport: transport)
            let persisted = try await client.providers()
            guard persisted.contains(where: {
                $0.peerId == transport.peerId &&
                    $0.certificateFingerprint == transport.certificateFingerprint
            }) else { throw AppModelError.providerBindingMismatch }
            providers = persisted
            return true
        } catch {
            report(error, title: "Backup device could not be added")
            return false
        }
    }

    public func connectProvider(signedTransportJSON: String) async -> Bool {
        let transport: PeerTransport
        do {
            transport = try decode(PeerTransport.self, from: signedTransportJSON)
            guard !transport.displayName.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
                  !transport.address.isEmpty,
                  !transport.address.contains(where: \Character.isWhitespace),
                  !transport.certificateDer.isEmpty
            else { throw AppModelError.providerBindingMismatch }
        } catch {
            report(error, title: "That connection file isn't valid")
            return false
        }
        return await connectProvider(using: transport)
    }

    public func isProviderReady(for pairing: NetworkPairing) -> Bool {
        guard pairing.state == .complete, let transport = pairing.peerTransport else { return false }
        return providers.contains {
            $0.peerId == transport.peerId &&
                $0.certificateFingerprint == transport.certificateFingerprint
        }
    }

    nonisolated static func validateProviderBinding(
        _ connected: ProviderConnection,
        transport: PeerTransport
    ) throws {
        let allowed = CharacterSet(charactersIn: "0123456789abcdef")
        guard transport.certificateFingerprint.utf8.count == 64,
              transport.certificateFingerprint.unicodeScalars.allSatisfy(allowed.contains),
              connected.peerId == transport.peerId,
              connected.certificateFingerprint == transport.certificateFingerprint
        else { throw AppModelError.providerBindingMismatch }
    }

    private func establishProviderConnection(for pairing: NetworkPairing) async throws {
        guard pairing.state == .complete, let transport = pairing.peerTransport else {
            throw AppModelError.providerBindingMismatch
        }
        let existing = try await client.providers().first { $0.peerId == transport.peerId }
        if let existing {
            try Self.validateProviderBinding(existing, transport: transport)
        } else {
            let connected = try await client.connectProvider(using: transport)
            try Self.validateProviderBinding(connected, transport: transport)
        }
        let persisted = try await client.providers()
        guard persisted.contains(where: {
            $0.peerId == transport.peerId &&
                $0.certificateFingerprint == transport.certificateFingerprint
        }) else { throw AppModelError.providerBindingMismatch }
        providers = persisted
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
        alertRetry = nil
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

    /// Presents a failure the way a person can act on it: plain-English lead,
    /// technical detail tucked behind a disclosure, and a real recovery button.
    ///
    /// `retry` is the operation to re-run when the recovery is ``RecoveryHint/retry``.
    /// Callers that can cheaply repeat themselves should pass it; without one,
    /// "Try Again" falls back to refreshing service state.
    private func report(
        _ error: Error,
        title: String,
        retry: (@MainActor () async -> Void)? = nil
    ) {
        let failure = ErrorPresenter.present(error)
        alertRetry = retry
        alert = AppAlert(
            title: title,
            message: failure.summary,
            detail: failure.detail,
            recovery: failure.recovery
        )
    }

    /// Removes the pending recovery from the model and hands it back ready to run.
    ///
    /// **Call this synchronously from the alert's button body.** SwiftUI writes
    /// `false` into the alert's `isPresented` binding the instant any button is
    /// tapped, and that setter calls ``clearAlert()``. A recovery that read
    /// `alert` from inside a deferred `Task` would therefore always find the
    /// alert already gone and silently do nothing.
    ///
    /// Returns `nil` when there is nothing to run — including
    /// ``RecoveryHint/checkNetworkSettings``, because opening system Settings
    /// is platform-specific and belongs to the iOS and macOS alert surfaces.
    public func takeAlertRecovery() -> (@MainActor () async -> Void)? {
        guard let alert else { return nil }
        let recovery = alert.recovery
        let retry = alertRetry
        clearAlert()
        switch recovery {
        case .checkNetworkSettings, .freeUpSpace, .none:
            return nil
        case .retry:
            return { [weak self] in
                guard let self else { return }
                if let retry {
                    await retry()
                } else {
                    await self.refresh()
                }
            }
        case .reconnect:
            return { [weak self] in self?.presentation = .connection }
        case .chooseAnotherDevice:
            return { [weak self] in
                self?.presentation = nil
                self?.selectedSection = .devices
            }
        case .chooseFolderAgain:
            // Settings only lists and revokes grants; the folder pickers live
            // on the Backups surface, so that is where "Choose Folder" leads.
            return { [weak self] in
                self?.presentation = nil
                self?.selectedSection = .backups
            }
        case .previewRestoreAgain:
            return { [weak self] in
                self?.dismissRestorePreview()
                self?.selectedSection = .backups
            }
        }
    }

    private static func snapshotIdentifier() -> String {
        let milliseconds = UInt64(Date().timeIntervalSince1970 * 1_000)
        return "s\(milliseconds)-\(UUID().uuidString.lowercased())"
    }
}

/// Holds the newest progress snapshot and admits one main-actor hop at a time.
///
/// `offer` returns `true` only when the caller should schedule a hop; every
/// other callback just updates the stored value, so a fast transfer costs one
/// pending hop rather than one per received chunk.
private final class ProgressCoalescer: @unchecked Sendable {
    private let lock = NSLock()
    private var pending: TransferProgressSnapshot?
    private var hopScheduled = false

    func offer(_ snapshot: TransferProgressSnapshot) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        pending = snapshot
        guard !hopScheduled else { return false }
        hopScheduled = true
        return true
    }

    func take() -> TransferProgressSnapshot? {
        lock.lock()
        defer { lock.unlock() }
        hopScheduled = false
        let snapshot = pending
        pending = nil
        return snapshot
    }
}

public enum AppModelError: Error, Equatable, Sendable {
    case notConnected
    case operationInProgress
    case noControllableOperation
    case folderPermissionMissing
    case providerNotConnected
    case providerCapacityUnverified
    case restorePreviewMissing
    case invalidDeviceName
    case invalidBackupName
    case invalidEndpoint
    case unsupportedSettings
    case settingsFileTooLarge
    case transferTooLarge
    case pairingNotMutuallyConfirmed
    case providerBindingMismatch
}

extension AppModelError: LocalizedError {
    public var errorDescription: String? {
        switch self {
        case .notConnected: "Connect to the local Covalent service first."
        case .operationInProgress: "Wait for the current backup, verification, or restore to finish."
        case .noControllableOperation: "The current operation cannot be paused or cancelled."
        case .folderPermissionMissing: "Choose the folder again to restore access."
        case .providerNotConnected: "One of the storage devices you chose is no longer connected."
        case .providerCapacityUnverified:
            "Every storage device you chose has to answer and have enough room before Covalent starts reading your folder."
        case .restorePreviewMissing: "Create a fresh restore preview before restoring files."
        case .invalidDeviceName: "Use a device name from 1 to 80 characters."
        case .invalidBackupName: "Use a backup name from 1 to 120 characters."
        case .invalidEndpoint: "Enter a reachable host and port for this device."
        case .unsupportedSettings: "This settings file uses an unsupported schema version."
        case .settingsFileTooLarge: "The settings file is larger than the 2 MiB limit."
        case .transferTooLarge: "The pairing transfer is larger than the 2 MiB limit."
        case .pairingNotMutuallyConfirmed: "Both devices must sign the matching code before pairing can finish."
        case .providerBindingMismatch: "The connected device did not match the transport identity signed during pairing."
        }
    }
}
