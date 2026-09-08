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
)

enum class FolderSyncAvailability { NOT_PACKAGED, NEEDS_ATTENTION, AVAILABLE }
enum class FolderSyncLifecycle { STOPPED, INITIAL_SCANNING, RUNNING, STILL_STOPPING, NEEDS_ATTENTION }
enum class FolderHealthFreshness { NEVER_OBSERVED, FRESH, STALE }
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
)

enum class FolderSharePhase { OFFERED, AWAITING_COMMIT, READY, PAUSED, REMOVED }

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
