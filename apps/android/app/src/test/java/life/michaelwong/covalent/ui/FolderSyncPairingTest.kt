package life.michaelwong.covalent.ui

import life.michaelwong.covalent.model.FolderHealthFreshness
import life.michaelwong.covalent.model.FolderSyncAvailability
import life.michaelwong.covalent.model.FolderSyncIssue
import life.michaelwong.covalent.model.FolderSyncLifecycle
import life.michaelwong.covalent.model.FolderSyncStatus
import life.michaelwong.covalent.model.PeerConnectionFreshness
import org.junit.Assert.assertEquals
import org.junit.Test

class FolderSyncPairingTest {
    @Test
    fun completedPairingStopsWaitingForUnavailableFolderSync() {
        assertEquals(
            FolderSyncPairingBlocker.INSTALLATION,
            status(
                availability = FolderSyncAvailability.NEEDS_ATTENTION,
                issue = FolderSyncIssue.INSTALLATION,
            ).pairingBlocker(),
        )
        assertEquals(
            FolderSyncPairingBlocker.INSTALLATION,
            status(availability = FolderSyncAvailability.NOT_PACKAGED).pairingBlocker(),
        )
        assertEquals(
            FolderSyncPairingBlocker.NEEDS_ATTENTION,
            status(
                availability = FolderSyncAvailability.NEEDS_ATTENTION,
                issue = FolderSyncIssue.FOLDER_ACCESS,
            ).pairingBlocker(),
        )
        assertEquals(null, status().pairingBlocker())
    }

    private fun status(
        availability: FolderSyncAvailability = FolderSyncAvailability.AVAILABLE,
        lifecycle: FolderSyncLifecycle = FolderSyncLifecycle.RUNNING,
        issue: FolderSyncIssue? = null,
    ) = FolderSyncStatus(
        availability = availability,
        lifecycle = lifecycle,
        issue = issue,
        healthFreshness = FolderHealthFreshness.NEVER_OBSERVED,
        peers = emptyList(),
        shares = emptyList(),
        folders = emptyList(),
        connectionFreshness = PeerConnectionFreshness.NEVER_OBSERVED,
    )
}
