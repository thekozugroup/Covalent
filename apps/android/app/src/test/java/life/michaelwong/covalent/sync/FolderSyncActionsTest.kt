package life.michaelwong.covalent.sync

import java.util.UUID
import life.michaelwong.covalent.model.FolderHealthFreshness
import life.michaelwong.covalent.model.FolderSyncAvailability
import life.michaelwong.covalent.model.FolderSyncLifecycle
import life.michaelwong.covalent.model.FolderSyncMutation
import life.michaelwong.covalent.model.FolderSyncStatus
import life.michaelwong.covalent.model.NodeConnection
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Test

class FolderSyncActionsTest {
    @Test
    fun offerPersistsFolderBeforeResolvingNodeOrCallingApi() {
        val events = mutableListOf<String>()
        val folder = UUID.fromString("11111111-1111-4111-8111-111111111111")
        val journal = RecordingJournal(events)
        val actions = FolderSyncActions(journal, RecordingApi(events)) {
            events += "node"
            CONNECTION
        }

        actions.offer("22222222-2222-4222-8222-222222222222", folder, "Photos", "/storage/emulated/0/Photos")

        assertEquals(listOf("prepare-offer", "node", "api-offer", "finish-offer"), events)
    }

    @Test
    fun acceptancePersistsDestinationBeforeNodeOrApiAndFailureLeavesItDurable() {
        val events = mutableListOf<String>()
        val journal = RecordingJournal(events)
        val actions = FolderSyncActions(journal, RecordingApi(events, failAccept = true)) {
            events += "node"
            CONNECTION
        }

        assertThrows(IllegalStateException::class.java) {
            actions.accept("33333333-3333-4333-8333-333333333333", "/storage/emulated/0/Shared")
        }
        assertEquals(listOf("prepare-accept", "node", "api-accept"), events)
    }

    @Test
    fun persistenceFailurePreventsNodeResolutionAndNetworkMutation() {
        val events = mutableListOf<String>()
        val journal = RecordingJournal(events, failPrepare = true)
        val actions = FolderSyncActions(journal, RecordingApi(events)) {
            events += "node"
            CONNECTION
        }
        assertThrows(IllegalStateException::class.java) {
            actions.accept("33333333-3333-4333-8333-333333333333", "/storage/emulated/0/Shared")
        }
        assertEquals(listOf("prepare-accept"), events)
    }

    private class RecordingJournal(
        private val events: MutableList<String>,
        private val failPrepare: Boolean = false,
    ) : FolderSyncGrantJournal {
        override fun records() = emptyList<FolderSyncGrant>()
        override fun prepareOffer(peerId: String, proposedFolderId: UUID, root: String, label: String): UUID {
            events += "prepare-offer"
            return proposedFolderId
        }
        override fun finishOffer(folderId: UUID, offerId: String) { events += "finish-offer" }
        override fun prepareAcceptance(offerId: String, root: String) {
            events += "prepare-accept"
            if (failPrepare) error("disk failure")
        }
        override fun remove(offerId: String) { events += "remove" }
        override fun reconcile(status: FolderSyncStatus) = Unit
    }

    private class RecordingApi(
        private val events: MutableList<String>,
        private val failAccept: Boolean = false,
    ) : FolderSyncApi {
        override fun status(connection: NodeConnection) = STATUS
        override fun offer(connection: NodeConnection, peerId: String, folderId: UUID, label: String, selectedRoot: String): FolderSyncMutation {
            events += "api-offer"
            return MUTATION
        }
        override fun accept(connection: NodeConnection, offerId: String, selectedRoot: String): FolderSyncMutation {
            events += "api-accept"
            if (failAccept) error("network failure")
            return MUTATION
        }
        override fun pause(connection: NodeConnection, offerId: String, paused: Boolean) = MUTATION
        override fun remove(connection: NodeConnection, offerId: String) = MUTATION
        override fun retry(connection: NodeConnection) = MUTATION
    }

    private companion object {
        val CONNECTION = NodeConnection("http://127.0.0.1:8787", "token")
        val MUTATION = FolderSyncMutation(
            "33333333-3333-4333-8333-333333333333",
            FolderSyncLifecycle.STOPPED,
            null,
        )
        val STATUS = FolderSyncStatus(
            FolderSyncAvailability.AVAILABLE,
            FolderSyncLifecycle.STOPPED,
            null,
            FolderHealthFreshness.NEVER_OBSERVED,
            emptyList(),
            emptyList(),
            emptyList(),
        )
    }
}
