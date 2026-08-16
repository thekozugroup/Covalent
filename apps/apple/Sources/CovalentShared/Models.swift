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

public struct RememberedBackup: Codable, Equatable, Sendable {
    public let backupId: UUID
    public let name: String
    public let ownerDeviceId: UUID

    public init(backupId: UUID, name: String, ownerDeviceId: UUID) {
        self.backupId = backupId
        self.name = name
        self.ownerDeviceId = ownerDeviceId
    }
}
