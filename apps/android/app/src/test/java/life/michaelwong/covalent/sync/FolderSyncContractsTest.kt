package life.michaelwong.covalent.sync

import life.michaelwong.covalent.model.FolderHealth
import life.michaelwong.covalent.model.FolderHealthFreshness
import life.michaelwong.covalent.model.FolderShare
import life.michaelwong.covalent.model.FolderSharePhase
import life.michaelwong.covalent.model.FolderShareSummary
import life.michaelwong.covalent.model.FolderSyncAvailability
import life.michaelwong.covalent.model.FolderSyncLifecycle
import life.michaelwong.covalent.model.FolderSyncPeer
import life.michaelwong.covalent.model.FolderSyncStatus
import life.michaelwong.covalent.model.PeerConnectionFreshness
import life.michaelwong.covalent.model.PeerConnectionState
import life.michaelwong.covalent.model.summaryFor
import org.junit.Assert.assertEquals
import org.junit.Test

class FolderSyncContractsTest {
    @Test
    fun errorsAndScanningPrecedeReachabilityWhileDisconnectNamesWaitingState() {
        val share = share(PeerConnectionState.DISCONNECTED)
        assertEquals(FolderShareSummary.WAITING_FOR_PEER, status(share).summaryFor(share))
        assertEquals(
            FolderShareSummary.CHECKING,
            status(share, lifecycle = FolderSyncLifecycle.INITIAL_SCANNING).summaryFor(share),
        )
        val errored = status(share).copy(folders = listOf(health.copy(statusError = true)))
        assertEquals(FolderShareSummary.NEEDS_ATTENTION, errored.summaryFor(share))
    }

    @Test
    fun staleOrMissingObservationNeverClaimsConnected() {
        val share = share(PeerConnectionState.CONNECTED)
        val stale = status(share).copy(connectionFreshness = PeerConnectionFreshness.STALE)
        assertEquals(FolderShareSummary.CONNECTION_UNKNOWN, stale.summaryFor(share))
        assertEquals(FolderShareSummary.CONNECTED, status(share).summaryFor(share))
    }

    private fun share(connection: PeerConnectionState) = FolderShare(
        offerId = "33333333-3333-4333-8333-333333333333",
        folderId = FOLDER,
        label = "Plans",
        peerId = PEER,
        incoming = false,
        phase = FolderSharePhase.READY,
        expiresAtUnixMs = null,
        expired = false,
        peerConnection = connection,
    )

    private fun status(
        share: FolderShare,
        lifecycle: FolderSyncLifecycle = FolderSyncLifecycle.RUNNING,
    ) = FolderSyncStatus(
        availability = FolderSyncAvailability.AVAILABLE,
        lifecycle = lifecycle,
        issue = null,
        healthFreshness = FolderHealthFreshness.FRESH,
        peers = listOf(FolderSyncPeer(PEER, "Kitchen server")),
        shares = listOf(share),
        folders = listOf(health),
        connectionFreshness = PeerConnectionFreshness.FRESH,
    )

    private companion object {
        const val PEER = "22222222-2222-4222-8222-222222222222"
        const val FOLDER = "11111111-1111-4111-8111-111111111111"
        val health = FolderHealth(FOLDER, "idle", 0, 0, 0, 0, false, false)
    }
}
