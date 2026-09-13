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

data class FolderSyncPeer(val peerId: String, val displayName: String, val address: String? = null)

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
    val remoteRemovalPending: Boolean = false,
    val waitingForConditions: Boolean = false,
    val pairingUpgradeRequired: Boolean = false,
    val linkPolicy: FolderLinkPolicy? = null,
    val linkSettings: FolderLinkSettingsState? = null,
    val linkRun: FolderLinkRunSummary? = null,
)

data class FolderLinkPolicy(
    val propagateSourceDeletions: Boolean = false,
    val restoreLocalDeletions: Boolean = false,
)

data class FolderLinkSettings(
    val deletionPolicy: FolderLinkPolicy,
    val paused: Boolean,
    val cadence: FolderLinkCadence = FolderLinkCadence.Continuous,
    val androidConditions: AndroidLinkConditions = AndroidLinkConditions(),
)

sealed interface FolderLinkCadence {
    data object Manual : FolderLinkCadence
    data object Continuous : FolderLinkCadence
    data class Scheduled(val intervalMinutes: Int) : FolderLinkCadence {
        init { require(intervalMinutes in 15..525_600) }
    }
}

data class AndroidLinkConditions(
    val wifiOnly: Boolean = false,
    val chargingOnly: Boolean = false,
)

enum class FolderLinkRunPhase { PREPARING, RUNNING, SUCCEEDED, INCOMPLETE, INTERRUPTED, CANCELLED }
enum class FolderLinkRunResult { PENDING, SUCCEEDED, FAILED, TIMED_OUT, INTERRUPTED, CANCELLED }
enum class FolderLinkRunRejectionReason { GENERATION_CHANGED, SETTINGS_CHANGED }

data class FolderLinkRunRequestSummary(val requestId: String, val requesterId: String)
data class FolderLinkRunRejection(
    val requesterId: String,
    val requestId: String,
    val reason: FolderLinkRunRejectionReason,
)
data class FolderLinkRunDestinationSummary(
    val peerId: String,
    val result: FolderLinkRunResult,
    val endedAtUnixMs: Long?,
)
data class FolderLinkRunSummary(
    val generation: Long,
    val stateRevision: Long,
    val settingsRevision: Long,
    val phase: FolderLinkRunPhase?,
    val startedAtUnixMs: Long?,
    val deadlineUnixMs: Long?,
    val endedAtUnixMs: Long?,
    val nextDueAtUnixMs: Long?,
    val pendingRequest: FolderLinkRunRequestSummary?,
    val rejectedRequest: FolderLinkRunRejection?,
    val destinations: List<FolderLinkRunDestinationSummary>,
)

data class FolderLinkSettingsChange(
    val folderId: String,
    val sourceId: String,
    val requesterId: String,
    val changeId: String,
    val expectedRevision: Long,
    val settings: FolderLinkSettings,
)

data class FolderLinkSettingsState(
    val revision: Long,
    val settings: FolderLinkSettings,
    val changeId: String,
    val changedBy: String,
    val confirmed: Boolean,
    val pendingChange: FolderLinkSettingsChange?,
    val conflictedChange: FolderLinkSettingsChange?,
    val acceptedAtUnixMs: Long = 0,
)

enum class FolderSharePhase { OFFERED, AWAITING_COMMIT, READY, PAUSED, REMOVED }

enum class FolderShareSummary {
    INVITATION_EXPIRED, PAUSED, CHECKING, NEEDS_ATTENTION, WAITING_FOR_OTHER_DEVICE,
    WAITING_FOR_CONDITIONS, OFFLINE, SYNCING, WAITING_FOR_PEER, CONNECTED, CONNECTION_UNKNOWN, READY,
    REMOVAL_PENDING, REMOVED,
}

/** UI summary with expiry, pause, scanning and explicit errors ahead of reachability. */
fun FolderSyncStatus.summaryFor(share: FolderShare): FolderShareSummary {
    if (share.phase == FolderSharePhase.REMOVED) {
        return if (share.remoteRemovalPending) FolderShareSummary.REMOVAL_PENDING else FolderShareSummary.REMOVED
    }
    if (share.expired) return FolderShareSummary.INVITATION_EXPIRED
    if (share.phase == FolderSharePhase.PAUSED) return FolderShareSummary.PAUSED
    if (folders.any { it.folderId == share.folderId && it.accessUnavailable }) {
        return FolderShareSummary.NEEDS_ATTENTION
    }
    if (lifecycle == FolderSyncLifecycle.INITIAL_SCANNING) return FolderShareSummary.CHECKING
    if (lifecycle == FolderSyncLifecycle.NEEDS_ATTENTION || issue != null) {
        return FolderShareSummary.NEEDS_ATTENTION
    }
    if (share.phase == FolderSharePhase.OFFERED || share.phase == FolderSharePhase.AWAITING_COMMIT) {
        return FolderShareSummary.WAITING_FOR_OTHER_DEVICE
    }
    if (share.waitingForConditions) return FolderShareSummary.WAITING_FOR_CONDITIONS
    if (lifecycle != FolderSyncLifecycle.RUNNING) {
        val settings = share.linkSettings
        val phase = share.linkRun?.phase
        if (availability == FolderSyncAvailability.AVAILABLE &&
            share.phase == FolderSharePhase.READY && settings?.confirmed == true &&
            settings.settings.cadence != FolderLinkCadence.Continuous &&
            phase !in setOf(FolderLinkRunPhase.PREPARING, FolderLinkRunPhase.RUNNING)
        ) return FolderShareSummary.READY
        return FolderShareSummary.OFFLINE
    }
    val health = folders.firstOrNull { it.folderId == share.folderId }
    if (healthFreshness == FolderHealthFreshness.FRESH && health != null) {
        if (health.statusError || health.watchError || health.scanPullErrorCount > 0 ||
            health.reportedErrorRows > 0 || health.state == "error"
        ) return FolderShareSummary.NEEDS_ATTENTION
        if (health.state in setOf("starting", "scanning", "scan-waiting")) {
            return FolderShareSummary.CHECKING
        }
        if (health.state in setOf("syncing", "sync-waiting", "sync-preparing") ||
            share.linkPolicy == null && (health.remainingFiles > 0 || health.remainingBytes > 0)
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
    val accessUnavailable: Boolean = false,
)

data class FolderSyncMutation(
    val offerId: String?,
    val lifecycle: FolderSyncLifecycle,
    val issue: FolderSyncIssue?,
)
