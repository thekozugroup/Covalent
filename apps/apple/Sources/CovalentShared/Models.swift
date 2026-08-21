import Foundation

public let covalentProtocolVersion: UInt16 = 1

public enum PlatformTier: String, Codable, Sendable {
    case tier1
    case tier2

    public var label: String {
        switch self {
        case .tier1: "Tier 1"
        case .tier2: "Tier 2"
        }
    }
}

public struct NodeStatus: Codable, Equatable, Sendable {
    public let deviceName: String
    public let protocolVersion: UInt16
    public let lanDiscovery: Bool
    public let platformTier: PlatformTier
    public let state: String

    public init(
        deviceName: String,
        protocolVersion: UInt16,
        lanDiscovery: Bool,
        platformTier: PlatformTier,
        state: String
    ) {
        self.deviceName = deviceName
        self.protocolVersion = protocolVersion
        self.lanDiscovery = lanDiscovery
        self.platformTier = platformTier
        self.state = state
    }
}

public struct TransportIdentity: Codable, Equatable, Sendable {
    public let deviceId: UUID
    public let peerPort: UInt16
    public let certificateDer: String
    public let certificateFingerprint: String
}

public enum DiscoverySource: String, Codable, Sendable {
    case lanMDNS = "lan_mdns"
    case tailscale

    public var label: String {
        switch self {
        case .lanMDNS: "Local network"
        case .tailscale: "Tailscale"
        }
    }
}

public struct DiscoveryCandidate: Codable, Equatable, Identifiable, Sendable {
    public let source: DiscoverySource
    public let endpoint: String
    public let serviceId: String
    public let minimumProtocolVersion: UInt16
    public let maximumProtocolVersion: UInt16

    public var id: String { "\(source.rawValue):\(serviceId):\(endpoint)" }
    public var isCompatible: Bool {
        minimumProtocolVersion <= covalentProtocolVersion
            && maximumProtocolVersion >= covalentProtocolVersion
    }
}

public struct ExportedDeviceSettings: Codable, Equatable, Sendable {
    public let schemaVersion: UInt16
    public let deviceName: String
    public let lanDiscoveryEnabled: Bool
    public let rememberedBackups: [RememberedBackup]

    public init(
        schemaVersion: UInt16 = 1,
        deviceName: String,
        lanDiscoveryEnabled: Bool,
        rememberedBackups: [RememberedBackup]
    ) {
        self.schemaVersion = schemaVersion
        self.deviceName = deviceName
        self.lanDiscoveryEnabled = lanDiscoveryEnabled
        self.rememberedBackups = rememberedBackups
    }
}

public struct RememberedBackup: Codable, Equatable, Identifiable, Sendable {
    public let backupId: UUID
    public let name: String
    public let ownerDeviceId: UUID

    public init(backupId: UUID, name: String, ownerDeviceId: UUID) {
        self.backupId = backupId
        self.name = name
        self.ownerDeviceId = ownerDeviceId
    }

    public var id: UUID { backupId }
}

public struct BackupSummary: Codable, Equatable, Identifiable, Sendable {
    public let backupId: UUID
    public let name: String
    public let ownerDeviceId: UUID
    public let latestSnapshotId: String?
    public let latestCommittedAtUnixMs: UInt64?
    public let snapshotCount: UInt64
    public let selectedProviderIds: [UUID]

    public var id: UUID { backupId }
}

public enum PeerRole: String, Codable, CaseIterable, Identifiable, Sendable {
    case storageProvider = "storage_provider"
    case backupReader = "backup_reader"
    case backupWriter = "backup_writer"

    public var id: String { rawValue }

    public var label: String {
        switch self {
        case .storageProvider: "Store extra copies"
        case .backupReader: "Restore this device's backups"
        case .backupWriter: "Add snapshots to this device's backups"
        }
    }
}

public struct PairingInvitation: Codable, Equatable, Sendable {
    public let protocolVersion: UInt16
    public let minimumProtocolVersion: UInt16
    public let inviterDeviceId: UUID
    public let inviterPublicKey: String
    public let inviterDeviceName: String
    public let invitationId: String
    public let invitationSecret: String
    public let invitationSecretCommitment: String
    public let expiresAtUnixMs: UInt64
    public let endpoints: [String]
    public let signature: String

    private enum CodingKeys: String, CodingKey {
        case protocolVersion
        case minimumProtocolVersion
        case inviterDeviceId
        case inviterPublicKey
        case inviterDeviceName
        case invitationId
        case invitationSecret
        case invitationSecretCommitment
        case expiresAtUnixMs
        case endpoints
        case signature
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        protocolVersion = try container.decode(UInt16.self, forKey: .protocolVersion)
        minimumProtocolVersion = try container.decodeIfPresent(UInt16.self, forKey: .minimumProtocolVersion) ?? 1
        inviterDeviceId = try container.decode(UUID.self, forKey: .inviterDeviceId)
        inviterPublicKey = try container.decode(String.self, forKey: .inviterPublicKey)
        inviterDeviceName = try container.decodeIfPresent(String.self, forKey: .inviterDeviceName) ?? ""
        invitationId = try container.decode(String.self, forKey: .invitationId)
        invitationSecret = try container.decodeIfPresent(String.self, forKey: .invitationSecret) ?? ""
        invitationSecretCommitment = try container.decodeIfPresent(String.self, forKey: .invitationSecretCommitment) ?? ""
        expiresAtUnixMs = try container.decode(UInt64.self, forKey: .expiresAtUnixMs)
        endpoints = try container.decode([String].self, forKey: .endpoints)
        signature = try container.decodeIfPresent(String.self, forKey: .signature) ?? ""
    }
}

public struct PairingSession: Codable, Equatable, Sendable {
    public let invitation: PairingInvitation
    public let responderDeviceId: UUID
    public let responderPublicKey: String
    public let responderName: String
    public let responderRoles: Set<PeerRole>
    public let inviterRoles: Set<PeerRole>
    public let authenticationString: String
    public let responderAcceptanceSignature: String
    public let responderConfirmationSignature: String?
    public let inviterConfirmationSignature: String?

    public var isMutuallySigned: Bool {
        responderConfirmationSignature != nil && inviterConfirmationSignature != nil
    }
}

public struct PeerGrant: Codable, Equatable, Identifiable, Sendable {
    public let peerDeviceId: UUID
    public let publicKey: String
    public let displayName: String
    public let roles: Set<PeerRole>
    public let confirmedAtUnixMs: UInt64
    public let revoked: Bool

    public var id: UUID { peerDeviceId }
}

public struct PairingConfirmation: Codable, Equatable, Sendable {
    public let inviterGrant: PeerGrant
    public let responderGrant: PeerGrant
    /// Caller-relative transport identity covered by the mutually signed pairing transcript.
    /// Older protocol-1 fixtures omit this additive field; the normal provider path requires it.
    public let peerTransport: PeerTransport?
}

public struct PeerTransport: Codable, Equatable, Sendable {
    public let peerId: UUID
    public let displayName: String
    public let address: String
    public let certificateDer: String
    public let certificateFingerprint: String
}

public enum NetworkPairingDirection: String, Codable, Sendable {
    case incoming
    case outgoing
}

public enum NetworkPairingState: String, Codable, Sendable {
    case awaitingLocalConfirmation = "awaiting_local_confirmation"
    case awaitingPeerConfirmation = "awaiting_peer_confirmation"
    case complete
    case failed
}

public struct NetworkPairing: Codable, Equatable, Identifiable, Sendable {
    public let pairingId: String
    public let direction: NetworkPairingDirection
    public let peerName: String
    public let authenticationString: String
    public let expiresAtUnixMs: UInt64
    public let state: NetworkPairingState
    public let failureCode: String?
    public let failureMessage: String?
    public let peerTransport: PeerTransport?

    public var id: String { pairingId }
}

public struct ProviderConnection: Codable, Equatable, Identifiable, Sendable {
    public let peerId: UUID
    public let address: String
    public let certificateFingerprint: String

    public var id: UUID { peerId }
}

public struct BackupRequest: Codable, Equatable, Sendable {
    public let sourceRoot: String
    public let backupId: UUID?
    public let displayName: String
    public let snapshotId: String
    public let jobId: String
    public let selectedProviderIds: [UUID]

    public init(
        sourceRoot: String,
        backupId: UUID? = nil,
        displayName: String,
        snapshotId: String,
        jobId: String,
        selectedProviderIds: [UUID]
    ) {
        self.sourceRoot = sourceRoot
        self.backupId = backupId
        self.displayName = displayName
        self.snapshotId = snapshotId
        self.jobId = jobId
        self.selectedProviderIds = selectedProviderIds
    }
}

public struct ArchiveBackupMetadata: Codable, Equatable, Sendable {
    public let protocolVersion: UInt16
    public let backupId: UUID?
    public let displayName: String
    public let snapshotId: String
    public let jobId: String
    public let selectedProviderIds: [UUID]

    public init(
        protocolVersion: UInt16 = covalentProtocolVersion,
        backupId: UUID? = nil,
        displayName: String,
        snapshotId: String,
        jobId: String,
        selectedProviderIds: [UUID]
    ) {
        self.protocolVersion = protocolVersion
        self.backupId = backupId
        self.displayName = displayName
        self.snapshotId = snapshotId
        self.jobId = jobId
        self.selectedProviderIds = selectedProviderIds
    }
}

public struct BackupResponse: Codable, Equatable, Sendable {
    public let backupId: UUID
    public let snapshotId: String
    public let entries: Int
    public let bytesRead: UInt64
    public let chunksStored: Int
    public let chunksDeduplicated: Int
    public let selectedProviders: Int
    public let degradedFailures: Int
}

public struct SnapshotRequest: Codable, Equatable, Sendable {
    public let backupId: UUID
    public let snapshotId: String
    public let verifyProviders: Bool
    public let repair: Bool

    public init(backupId: UUID, snapshotId: String, verifyProviders: Bool, repair: Bool) {
        self.backupId = backupId
        self.snapshotId = snapshotId
        self.verifyProviders = verifyProviders
        self.repair = repair
    }
}

public enum ReplicaAvailability: String, Codable, Sendable {
    case complete
    case degraded
    case offline
    case corrupt
    case revoked
}

public struct VerifyResponse: Codable, Equatable, Sendable {
    public let verified: Int
    public let missing: [String]
    public let corrupt: [String]
    public let intact: Bool
    public let providerAvailability: [String: ReplicaAvailability]
}

public enum ConflictPolicy: String, Codable, CaseIterable, Identifiable, Sendable {
    case fail
    case skip
    case replace
    case rename

    public var id: String { rawValue }

    public var label: String {
        switch self {
        case .fail: "Stop if a file exists"
        case .skip: "Keep existing files"
        case .replace: "Replace existing files"
        case .rename: "Keep both copies"
        }
    }

    public var isDestructive: Bool { self == .replace }

    public var safetyDetail: String {
        switch self {
        case .fail: "Safest. Nothing is written if any destination file already exists."
        case .skip: "Existing files stay unchanged; only missing items are restored."
        case .replace: "Existing files in the signed plan can be overwritten after confirmation."
        case .rename: "Existing files stay unchanged and restored copies receive a new name."
        }
    }
}

public struct RestorePreviewRequest: Codable, Equatable, Sendable {
    public let backupId: UUID
    public let snapshotId: String
    public let targetRoot: String
    public let conflictPolicy: ConflictPolicy
    public let jobId: String
}

public enum RestoreEntryKind: String, Codable, Sendable {
    case file
    case directory
}

public enum RestoreAction: String, Codable, Sendable {
    case createFile = "create_file"
    case createDirectory = "create_directory"
    case keepDirectory = "keep_directory"
    case skipFile = "skip_file"
    case replaceFile = "replace_file"
    case renameFile = "rename_file"
}

public struct RestorePreviewEntry: Codable, Equatable, Identifiable, Sendable {
    public let sourcePath: String
    public let destinationPath: String
    public let kind: RestoreEntryKind
    public let action: RestoreAction

    public var id: String { "\(destinationPath):\(action.rawValue)" }
}

public struct TargetInventoryEntry: Codable, Equatable, Sendable {
    public let path: String
    public let kind: RestoreEntryKind
    public let length: UInt64
    public let modifiedAtUnixMs: UInt64?
    public let identityToken: String
}

public struct TargetInventoryBinding: Codable, Equatable, Sendable {
    public let schemaVersion: UInt16
    public let rootIdentity: String
    public let entryCount: UInt64
    public let totalBytes: UInt64
    public let inventoryDigest: String
    public let actionsDigest: String
}

public struct RestorePlanReference: Codable, Equatable, Sendable {
    public let planId: String
    public let backupId: UUID
    public let snapshotId: String
    public let authorizedRoot: String
    public let manifestDigest: String
    public let conflictPolicy: ConflictPolicy
    public let jobId: String
    public let planDigest: String
    public let signerDeviceId: UUID
    public let signature: String
    public let totalEntries: Int
    public let targetInventory: TargetInventoryBinding?
}

public struct RestorePlanPage: Codable, Equatable, Sendable {
    public let planId: String
    public let backupId: UUID
    public let snapshotId: String
    public let authorizedRoot: String
    public let manifestDigest: String
    public let conflictPolicy: ConflictPolicy
    public let jobId: String
    public let planDigest: String
    public let signerDeviceId: UUID
    public let signature: String
    public let entryOffset: Int
    public let totalEntries: Int
    public let entries: [RestorePreviewEntry]
    public let nextCursor: String?
    public let targetInventory: TargetInventoryBinding?
}

public struct RestorePlan: Equatable, Sendable {
    public let reference: RestorePlanReference
    public let entries: [RestorePreviewEntry]

    public var planId: String { reference.planId }
    public var backupId: UUID { reference.backupId }
    public var snapshotId: String { reference.snapshotId }
    public var authorizedRoot: String { reference.authorizedRoot }
    public var manifestDigest: String { reference.manifestDigest }
    public var conflictPolicy: ConflictPolicy { reference.conflictPolicy }
    public var jobId: String { reference.jobId }
    public var planDigest: String { reference.planDigest }
    public var signerDeviceId: UUID { reference.signerDeviceId }
    public var signature: String { reference.signature }
    public var totalEntries: Int { reference.totalEntries }
    public var targetInventory: TargetInventoryBinding? { reference.targetInventory }

    public init(reference: RestorePlanReference, entries: [RestorePreviewEntry]) {
        self.reference = reference
        self.entries = entries
    }
}

public struct RestoreResponse: Codable, Equatable, Sendable {
    public let filesRestored: Int
    public let directoriesCreated: Int
    public let filesSkipped: Int
    public let bytesWritten: UInt64
    public let rejectedProviderCopies: Int
}

public enum JobAction: String, Codable, Sendable {
    case pause
    case resume
    case cancel
}

public enum JobState: String, Codable, Sendable {
    case running
    case paused
    case cancelled
}

public struct JobControlResponse: Codable, Equatable, Sendable {
    public let jobId: String
    public let state: JobState
}

public struct APIErrorPayload: Codable, Equatable, Sendable {
    public let protocolVersion: UInt16
    public let code: String
    public let message: String
    public let retryable: Bool
}

public enum TransferKind: String, Codable, Sendable {
    case backup
    case verification
    case restore
}

public enum TransferState: String, Codable, Sendable {
    case queued
    case running
    case paused
    case completed
    case failed
    case cancelled
}

/// Live progress for a transfer this device is driving.
///
/// The node exposes no progress-polling route, so — exactly as the Android
/// client does — these counts come from the bytes this device has actually
/// put on (or taken off) the wire. `totalBytes` is `nil` only while the work
/// genuinely has no known size, which is what keeps the indeterminate
/// spinner honest instead of universal.
public struct TransferProgressSnapshot: Equatable, Sendable {
    public enum Phase: String, Equatable, Sendable {
        /// Reading and encrypting locally; the byte total isn't known yet.
        case preparing
        /// Bytes are moving. `completedBytes` and `totalBytes` are real.
        case transferring
        /// The transfer landed; the node is finalising it.
        case finishing

        public var label: String {
            switch self {
            case .preparing: "Preparing"
            case .transferring: "Transferring"
            case .finishing: "Finishing up"
            }
        }
    }

    public let phase: Phase
    public let completedBytes: UInt64
    public let totalBytes: UInt64?

    public init(phase: Phase, completedBytes: UInt64 = 0, totalBytes: UInt64? = nil) {
        self.phase = phase
        self.completedBytes = completedBytes
        self.totalBytes = totalBytes
    }

    /// `nil` when the total is genuinely unknown — callers must fall back to
    /// an indeterminate indicator rather than inventing a denominator.
    public var fractionCompleted: Double? {
        guard let totalBytes, totalBytes > 0 else { return nil }
        return min(1, Double(completedBytes) / Double(totalBytes))
    }

    /// "1.2 GB of 8.4 GB", or just the completed count when no total is known.
    ///
    /// Clamped to the total so a late or over-reported byte count can never
    /// render the nonsense "8.5 GB of 8.4 GB".
    public var byteSummary: String {
        guard let totalBytes, totalBytes > 0 else {
            return completedBytes.formatted(.byteCount(style: .file))
        }
        let shown = min(completedBytes, totalBytes)
        return "\(shown.formatted(.byteCount(style: .file))) of \(totalBytes.formatted(.byteCount(style: .file)))"
    }
}

public struct TransferProgress: Codable, Equatable, Sendable {
    public let protocolVersion: UInt16
    public let jobId: String
    public let kind: TransferKind
    public let state: TransferState
    public let completedBytes: UInt64
    public let totalBytes: UInt64?
    public let completedEntries: UInt64
    public let message: String
}

public enum NodeEventKind: String, Codable, Sendable {
    case transferChanged = "transfer_changed"
    case peerChanged = "peer_changed"
    case settingsChanged = "settings_changed"
}

public struct NodeEvent: Codable, Equatable, Sendable {
    public let protocolVersion: UInt16
    public let sequence: UInt64
    public let occurredAtUnixMs: UInt64
    public let kind: NodeEventKind
    public let jobId: String?
    public let message: String
}

public enum SnapshotIntegrity: String, Codable, Sendable {
    case unknown
    case checking
    case intact
    case degraded
    case corrupt

    public var label: String {
        switch self {
        case .unknown: "Not checked"
        case .checking: "Checking"
        case .intact: "Verified"
        case .degraded: "Needs attention"
        case .corrupt: "Corrupt"
        }
    }
}

public struct SnapshotRecord: Codable, Equatable, Identifiable, Sendable {
    public let id: UUID
    public let backupId: UUID
    public let displayName: String
    public let snapshotId: String
    public let createdAt: Date
    public let sourceGrantId: UUID
    public let selectedProviderIds: [UUID]
    public let entries: Int
    public let bytesRead: UInt64
    public let chunksStored: Int
    public let chunksDeduplicated: Int
    public let degradedFailures: Int
    public var integrity: SnapshotIntegrity

    public init(
        id: UUID = UUID(),
        backupId: UUID,
        displayName: String,
        snapshotId: String,
        createdAt: Date = Date(),
        sourceGrantId: UUID,
        selectedProviderIds: [UUID],
        response: BackupResponse,
        integrity: SnapshotIntegrity = .unknown
    ) {
        self.id = id
        self.backupId = backupId
        self.displayName = displayName
        self.snapshotId = snapshotId
        self.createdAt = createdAt
        self.sourceGrantId = sourceGrantId
        self.selectedProviderIds = selectedProviderIds
        self.entries = response.entries
        self.bytesRead = response.bytesRead
        self.chunksStored = response.chunksStored
        self.chunksDeduplicated = response.chunksDeduplicated
        self.degradedFailures = response.degradedFailures
        self.integrity = integrity
    }
}
