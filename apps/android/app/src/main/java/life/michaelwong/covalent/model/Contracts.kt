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
    val displayName: String? = null,
    val roles: Set<String> = emptySet(),
    val reachability: ProviderReachability = ProviderReachability.UNKNOWN,
    val capacityBytes: Long? = null,
)

enum class ProviderReachability { CONNECTED, OFFLINE, UNKNOWN }

data class TransportIdentity(
    val deviceId: String,
    val peerPort: Int,
    val certificateDer: String,
    val certificateFingerprint: String,
)

enum class NetworkPairingDirection { INCOMING, OUTGOING }

enum class NetworkPairingState {
    AWAITING_LOCAL_CONFIRMATION,
    AWAITING_PEER_CONFIRMATION,
    COMPLETE,
    FAILED,
}

data class PeerTransport(
    val peerId: String,
    val displayName: String,
    val address: String,
    val certificateDer: String,
    val certificateFingerprint: String,
)

data class NetworkPairing(
    val pairingId: String,
    val direction: NetworkPairingDirection,
    val peerName: String,
    val authenticationString: String,
    val expiresAtUnixMs: Long,
    val state: NetworkPairingState,
    val failureCode: String?,
    val failureMessage: String?,
    val peerTransport: PeerTransport?,
)

data class PeerGrant(
    val peerDeviceId: String,
    val displayName: String,
    val roles: Set<String>,
    val revoked: Boolean,
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

enum class RestoreConflictPolicy(val wireValue: String) {
    FAIL("fail"),
    SKIP("skip"),
    RENAME("rename"),
}

data class TargetInventoryEntry(
    val path: String,
    val kind: String,
    val length: Long,
    val modifiedAtUnixMs: Long?,
    val identityToken: String,
)

data class TargetInventoryDraft(
    val rootIdentity: String,
    val totalBytes: Long,
    val entries: List<TargetInventoryEntry>,
)

data class TargetInventoryReference(
    val inventoryId: String,
    val jobId: String,
    val schemaVersion: Int,
    val rootIdentity: String,
    val entryCount: Long,
    val totalBytes: Long,
    val inventoryDigest: String,
)

data class TargetInventoryBinding(
    val schemaVersion: Int,
    val rootIdentity: String,
    val entryCount: Long,
    val totalBytes: Long,
    val inventoryDigest: String,
    val actionsDigest: String,
)

/**
 * Small, durable handle returned by restore preview. The complete signed entry list stays on the
 * node and is fetched in bounded pages; legacyPlanJson exists only for a compatibility window with
 * nodes that still return the former inline plan contract.
 */
data class RestorePlanReference(
    val planId: String?,
    val planDigest: String,
    val backupId: String,
    val snapshotId: String,
    val authorizedRoot: String,
    val manifestDigest: String,
    val conflictPolicy: String,
    val jobId: String,
    val signerDeviceId: String,
    val signature: String,
    val totalEntries: Long,
    val targetInventory: TargetInventoryBinding? = null,
    val legacyPlanJson: String? = null,
)

data class RestorePreviewEntry(
    val destinationPath: String,
    val kind: String,
    val action: String,
    val sourcePath: String = destinationPath,
)

data class RestorePlanPage(
    val reference: RestorePlanReference,
    val entryOffset: Long,
    val entries: List<RestorePreviewEntry>,
    val nextCursor: String?,
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
    val completedBytes: Long = 0,
    val totalBytes: Long? = null,
    val completedEntries: Long = 0,
    val totalEntries: Long? = null,
    val updatedAtUnixMs: Long = System.currentTimeMillis(),
    val retryable: Boolean = false,
)

enum class TransferKind { BACKUP, VERIFICATION, RESTORE }

enum class TransferState { QUEUED, RUNNING, PAUSED, COMPLETED, FAILED, CANCELLED }

enum class PrimaryAction { PAIR, BACKUP, RESTORE }
