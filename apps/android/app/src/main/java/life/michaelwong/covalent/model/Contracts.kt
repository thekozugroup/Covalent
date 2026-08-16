package life.michaelwong.covalent.model

enum class PlatformTier(val label: String) {
    TIER_1("Tier 1"),
    TIER_2("Tier 2"),
}

data class NodeStatus(
    val deviceName: String,
    val protocolVersion: UShort,
    val lanDiscovery: Boolean,
    val platformTier: PlatformTier,
    val state: String,
)

data class NodeConnection(
    val baseUrl: String,
    val token: String,
    val status: NodeStatus? = null,
)

data class DiscoveryCandidate(
    val source: String,
    val endpoint: String,
    val serviceId: String,
)

data class Provider(
    val peerId: String,
    val address: String,
    val fingerprint: String,
)

data class RememberedBackup(
    val backupId: String,
    val name: String,
    val ownerDeviceId: String,
    val latestSnapshotId: String?,
    val latestCommittedAtUnixMs: Long?,
    val snapshotCount: Long,
    val selectedProviderIds: Set<String>,
)

data class ApiErrorPayload(
    val protocolVersion: UShort,
    val code: String,
    val message: String,
    val retryable: Boolean,
)

data class TransferProgress(
    val protocolVersion: UShort,
    val jobId: String,
    val kind: TransferKind,
    val state: TransferState,
    val completedBytes: Long,
    val totalBytes: Long?,
    val completedEntries: Long,
    val message: String,
)

enum class NodeEventKind { TRANSFER_CHANGED, PEER_CHANGED, SETTINGS_CHANGED }

data class NodeEvent(
    val protocolVersion: UShort,
    val sequence: Long,
    val occurredAtUnixMs: Long,
    val kind: NodeEventKind,
    val jobId: String?,
    val message: String,
)

data class TransferRecord(
    val jobId: String,
    val label: String,
    val kind: TransferKind,
    val state: TransferState,
    val detail: String = "",
)

enum class TransferKind { BACKUP, VERIFICATION, RESTORE }

enum class TransferState { QUEUED, RUNNING, PAUSED, COMPLETED, FAILED, CANCELLED }

enum class PrimaryAction(val label: String) {
    PAIR("Pair"),
    BACKUP("Backup"),
    RESTORE("Restore"),
}
