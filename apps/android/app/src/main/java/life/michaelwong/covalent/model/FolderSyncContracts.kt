package life.michaelwong.covalent.model

/** Redacted lifecycle state returned by the authenticated local folder-sync API. */
data class FolderSyncStatus(
    val availability: FolderSyncAvailability,
    val lifecycle: FolderSyncLifecycle,
    val issue: FolderSyncIssue?,
    val healthFreshness: FolderHealthFreshness,
    val peers: List<FolderSyncPeer>,
    val shares: List<FolderShare>,
    val folders: List<FolderHealth>,
    val connectionFreshness: PeerConnectionFreshness = PeerConnectionFreshness.NEVER_OBSERVED,
)

enum class FolderSyncAvailability { NOT_PACKAGED, NEEDS_ATTENTION, AVAILABLE }
enum class FolderSyncLifecycle { STOPPED, INITIAL_SCANNING, RUNNING, STILL_STOPPING, NEEDS_ATTENTION }
enum class FolderHealthFreshness { NEVER_OBSERVED, FRESH, STALE }
enum class PeerConnectionFreshness { NEVER_OBSERVED, FRESH, STALE }
enum class PeerConnectionState { UNKNOWN, CONNECTED, DISCONNECTED, PAUSED }
enum class FolderSyncIssue {
    INSTALLATION, FOLDER_ACCESS, INITIAL_SCAN, JOURNAL, WORKER_LAUNCH, WORKER_HEALTH, WORKER_STOP, PEER_REVOCATION,
}

data class FolderSyncPeer(val peerId: String, val displayName: String)

data class FolderShare(
    val offerId: String,
    val folderId: String,
    val label: String,
    val peerId: String,
    val incoming: Boolean,
    val phase: FolderSharePhase,
    val expiresAtUnixMs: Long?,
    val expired: Boolean,
    val peerConnection: PeerConnectionState = PeerConnectionState.UNKNOWN,
    val supersededOfferIds: List<String> = emptyList(),
)

enum class FolderSharePhase { OFFERED, AWAITING_COMMIT, READY, PAUSED, REMOVED }

enum class FolderShareSummary {
    INVITATION_EXPIRED, PAUSED, CHECKING, NEEDS_ATTENTION, WAITING_FOR_OTHER_DEVICE,
    OFFLINE, SYNCING, WAITING_FOR_PEER, CONNECTED, CONNECTION_UNKNOWN,
}

/** UI summary with expiry, pause, scanning and explicit errors ahead of reachability. */
fun FolderSyncStatus.summaryFor(share: FolderShare): FolderShareSummary {
    if (share.expired) return FolderShareSummary.INVITATION_EXPIRED
    if (share.phase == FolderSharePhase.PAUSED) return FolderShareSummary.PAUSED
    if (lifecycle == FolderSyncLifecycle.INITIAL_SCANNING) return FolderShareSummary.CHECKING
    if (lifecycle == FolderSyncLifecycle.NEEDS_ATTENTION || issue != null) {
        return FolderShareSummary.NEEDS_ATTENTION
    }
    if (share.phase == FolderSharePhase.OFFERED || share.phase == FolderSharePhase.AWAITING_COMMIT) {
        return FolderShareSummary.WAITING_FOR_OTHER_DEVICE
    }
    if (lifecycle != FolderSyncLifecycle.RUNNING) return FolderShareSummary.OFFLINE
    val health = folders.firstOrNull { it.folderId == share.folderId }
    if (healthFreshness == FolderHealthFreshness.FRESH && health != null) {
        if (health.statusError || health.watchError || health.scanPullErrorCount > 0 ||
            health.reportedErrorRows > 0 || health.state == "error"
        ) return FolderShareSummary.NEEDS_ATTENTION
        if (health.state in setOf("starting", "scanning", "scan-waiting")) {
            return FolderShareSummary.CHECKING
        }
        if (health.state in setOf("syncing", "sync-waiting", "sync-preparing") ||
            health.remainingFiles > 0 || health.remainingBytes > 0
        ) return FolderShareSummary.SYNCING
    }
    return if (connectionFreshness != PeerConnectionFreshness.FRESH) {
        FolderShareSummary.CONNECTION_UNKNOWN
    } else when (share.peerConnection) {
        PeerConnectionState.CONNECTED -> FolderShareSummary.CONNECTED
        PeerConnectionState.DISCONNECTED -> FolderShareSummary.WAITING_FOR_PEER
        PeerConnectionState.PAUSED -> FolderShareSummary.PAUSED
        PeerConnectionState.UNKNOWN -> FolderShareSummary.CONNECTION_UNKNOWN
    }
}

data class FolderHealth(
    val folderId: String,
    val state: String,
    val remainingFiles: Long,
    val remainingBytes: Long,
    val scanPullErrorCount: Long,
    val reportedErrorRows: Int,
    val statusError: Boolean,
    val watchError: Boolean,
)

data class FolderSyncMutation(
    val offerId: String?,
    val lifecycle: FolderSyncLifecycle,
    val issue: FolderSyncIssue?,
)
