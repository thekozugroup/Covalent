package life.michaelwong.covalent.sync

import java.util.UUID
import life.michaelwong.covalent.model.FolderHealthFreshness
import life.michaelwong.covalent.model.FolderShare
import life.michaelwong.covalent.model.FolderLinkPolicy
import life.michaelwong.covalent.model.FolderLinkSettings
import life.michaelwong.covalent.model.FolderSharePhase
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
        val api = RecordingApi(events)
        val actions = actions(events, journal, api) { "/validated$it" }

        val policy = FolderLinkPolicy(propagateSourceDeletions = true, restoreLocalDeletions = false)
        actions.offer(
            "22222222-2222-4222-8222-222222222222",
            folder,
            "Photos",
            "/storage/emulated/0/Photos",
            policy,
        )

        assertEquals(listOf("validate-root", "prepare-offer", "node", "api-offer", "finish-offer"), events)
        assertEquals("/validated/storage/emulated/0/Photos", journal.offeredRoot)
        assertEquals("/validated/storage/emulated/0/Photos", api.offeredRoot)
        assertEquals(policy, api.offeredPolicy)
    }

    @Test
    fun addDestinationReusesTheSavedSourceIdentityAndPolicy() {
        val events = mutableListOf<String>()
        val policy = FolderLinkPolicy(propagateSourceDeletions = false, restoreLocalDeletions = true)
        val source = FolderShare(
            OFFER,
            SOURCE_FOLDER,
            "Photos",
            SOURCE_PEER,
            false,
            FolderSharePhase.READY,
            null,
            false,
            linkPolicy = policy,
        )
        val journal = RecordingJournal(events).apply {
            saved = listOf(FolderSyncGrant(
                "offer:$OFFER",
                FolderSyncGrantKind.OFFER,
                OFFER,
                SOURCE_FOLDER,
                SOURCE_PEER,
                "/storage/emulated/0/Photos",
                "Photos",
            ))
        }
        val api = RecordingApi(events)

        actions(events, journal, api).addDestination(source, OTHER_OFFER)

        assertEquals(listOf("validate-root", "prepare-offer", "node", "api-offer", "finish-offer"), events)
        assertEquals(UUID.fromString(SOURCE_FOLDER), api.offeredFolderId)
        assertEquals(OTHER_OFFER, api.offeredPeerId)
        assertEquals("Photos", api.offeredLabel)
        assertEquals("/storage/emulated/0/Photos", api.offeredRoot)
        assertEquals(policy, api.offeredPolicy)
    }

    @Test
    fun acceptancePersistsDestinationBeforeNodeOrApiAndFailureLeavesItDurable() {
        val events = mutableListOf<String>()
        val journal = RecordingJournal(events)
        val actions = actions(events, journal, RecordingApi(events, failAccept = true))

        assertThrows(IllegalStateException::class.java) {
            actions.accept("33333333-3333-4333-8333-333333333333", "/storage/emulated/0/Shared")
        }
        assertEquals(listOf("validate-root", "prepare-accept", "node", "api-accept"), events)
    }

    @Test
    fun persistenceFailurePreventsNodeResolutionAndNetworkMutation() {
        val events = mutableListOf<String>()
        val journal = RecordingJournal(events, failPrepare = true)
        val actions = actions(events, journal, RecordingApi(events))
        assertThrows(IllegalStateException::class.java) {
            actions.accept("33333333-3333-4333-8333-333333333333", "/storage/emulated/0/Shared")
        }
        assertEquals(listOf("validate-root", "prepare-accept"), events)
    }

    @Test
    fun repairValidatesAndPersistsPendingRootBeforeMutationThenAcknowledgesAndRestarts() {
        val events = mutableListOf<String>()
        val journal = RecordingJournal(events)
        val actions = actions(events, journal, RecordingApi(events))

        actions.repair(OFFER, "/storage/emulated/0/New")

        assertEquals(
            listOf("validate-root", "prepare-repair", "node", "api-repair", "finish-repair", "restart"),
            events,
        )
    }

    @Test
    fun failedRepairRetainsExactSavedRootAndRetryNeverUsesANewCallerValue() {
        val events = mutableListOf<String>()
        val journal = RecordingJournal(events)
        val failed = actions(events, journal, RecordingApi(events, failRepair = true))
        assertThrows(IllegalStateException::class.java) {
            failed.repair(OFFER, "/storage/emulated/0/New")
        }
        assertEquals("/storage/emulated/0/New", journal.pendingRepairRoot(OFFER))

        events.clear()
        actions(events, journal, RecordingApi(events)).retryRepair(OFFER)
        assertEquals(
            listOf("validate-root", "prepare-repair", "node", "api-repair", "finish-repair", "restart"),
            events,
        )
    }

    @Test
    fun removalTombstonePrecedesNodeAndIsClearedOnlyAfterExactAcknowledgement() {
        val events = mutableListOf<String>()
        val journal = RecordingJournal(events)
        val mismatch = actions(events, journal, RecordingApi(events, responseOffer = OTHER_OFFER))
        assertThrows(IllegalStateException::class.java) { mismatch.remove(OFFER) }
        assertEquals(listOf("prepare-remove", "node", "api-remove"), events)
        assertEquals(true, journal.removalPending)

        events.clear()
        actions(events, journal, RecordingApi(events)).remove(OFFER)
        assertEquals(listOf("prepare-remove", "node", "api-remove", "finish-remove"), events)
        assertEquals(false, journal.removalPending)
    }

    @Test
    fun renewalRequiresSavedSenderChoiceAndAcknowledgedReplacementRelationship() {
        val events = mutableListOf<String>()
        val journal = RecordingJournal(events)
        val api = RecordingApi(events, responseOffer = OTHER_OFFER)
        assertThrows(IllegalStateException::class.java) { actions(events, journal, api).renew(OFFER) }
        assertEquals(emptyList<String>(), events)
        journal.saved = listOf(FolderSyncGrant(
            "offer:$OFFER", FolderSyncGrantKind.OFFER, OFFER, OTHER_OFFER, OTHER_OFFER,
            "/storage/emulated/0/Photos", "Photos",
        ))
        assertThrows(IllegalStateException::class.java) { actions(events, journal, api).renew(OFFER) }
        assertEquals(listOf("node", "api-renew"), events)
        assertEquals(OFFER, journal.saved.single().offerId)
        events.clear()
        val confirmed = STATUS.copy(shares = listOf(FolderShare(
            OTHER_OFFER, OTHER_OFFER, "Photos", OTHER_OFFER, false,
            FolderSharePhase.OFFERED, 100, false, supersededOfferIds = listOf(OFFER),
        )))
        val result = actions(events, journal, RecordingApi(
            events, responseOffer = OTHER_OFFER, responseStatus = confirmed,
        )).renew(OFFER)
        assertEquals(OTHER_OFFER, result.offerId)
        assertEquals(listOf("node", "api-renew", "reconcile"), events)
        assertEquals(confirmed, journal.reconciledStatus)
    }

    @Test
    fun addressRefreshUsesTheExactCapturedRequestAndRequiresANonShareResponse() {
        val events = mutableListOf<String>()
        val request = PeerAddressRefreshRequest(
            OTHER_OFFER,
            "192.0.2.10:8787",
            "192.0.2.11:8787",
        )
        val api = RecordingApi(events)

        val result = actions(events, RecordingJournal(events), api).refreshPeerAddress(request)

        assertEquals(listOf("node", "api-refresh-address"), events)
        assertEquals(request, api.addressRequest)
        assertEquals(null, result.offerId)

        events.clear()
        val invalid = RecordingApi(events, addressResponseOffer = OFFER)
        assertThrows(IllegalStateException::class.java) {
            actions(events, RecordingJournal(events), invalid).refreshPeerAddress(request)
        }
        assertEquals(listOf("node", "api-refresh-address"), events)
    }

    private fun actions(
        events: MutableList<String>,
        journal: RecordingJournal,
        api: RecordingApi,
        normalizeRoot: (String) -> String = { it },
    ) = FolderSyncActions(
        grants = journal,
        api = api,
        ensureNodeReady = {
            events += "node"
            CONNECTION
        },
        validateRoot = {
            events += "validate-root"
            normalizeRoot(it)
        },
        onRepairAcknowledged = { events += "restart" },
    )

    private class RecordingJournal(
        private val events: MutableList<String>,
        private val failPrepare: Boolean = false,
    ) : FolderSyncGrantJournal {
        private var pendingRepair: String? = null
        var offeredRoot: String? = null
            private set
        var removalPending = false
            private set
        var saved = emptyList<FolderSyncGrant>()
        var reconciledStatus: FolderSyncStatus? = null

        override fun records() = saved
        override fun prepareOffer(peerId: String, proposedFolderId: UUID, root: String, label: String): UUID {
            events += "prepare-offer"
            offeredRoot = root
            return proposedFolderId
        }
        override fun finishOffer(folderId: UUID, offerId: String) { events += "finish-offer" }
        override fun prepareAcceptance(offerId: String, root: String) {
            events += "prepare-accept"
            if (failPrepare) error("disk failure")
        }
        override fun prepareRepair(offerId: String, root: String) {
            events += "prepare-repair"
            check(pendingRepair == null || pendingRepair == root)
            pendingRepair = root
        }
        override fun finishRepair(offerId: String, root: String) {
            events += "finish-repair"
            check(pendingRepair == root)
            pendingRepair = null
        }
        override fun pendingRepairRoot(offerId: String): String? = pendingRepair
        override fun prepareRemoval(offerId: String) {
            events += "prepare-remove"
            removalPending = true
        }
        override fun finishRemoval(offerId: String) {
            events += "finish-remove"
            check(removalPending)
            removalPending = false
        }
        override fun reconcile(status: FolderSyncStatus) {
            events += "reconcile"
            reconciledStatus = status
        }
    }

    private class RecordingApi(
        private val events: MutableList<String>,
        private val failAccept: Boolean = false,
        private val failRepair: Boolean = false,
        private val responseOffer: String = OFFER,
        private val responseStatus: FolderSyncStatus = STATUS,
        private val addressResponseOffer: String? = null,
    ) : FolderSyncApi {
        var offeredRoot: String? = null
            private set
        var offeredPolicy: FolderLinkPolicy? = null
            private set
        var offeredFolderId: UUID? = null
            private set
        var offeredPeerId: String? = null
            private set
        var offeredLabel: String? = null
            private set
        var addressRequest: PeerAddressRefreshRequest? = null
            private set
        override fun status(connection: NodeConnection) = responseStatus
        override fun offer(
            connection: NodeConnection,
            peerId: String,
            folderId: UUID,
            label: String,
            selectedRoot: String,
            linkPolicy: FolderLinkPolicy,
        ): FolderSyncMutation {
            events += "api-offer"
            offeredFolderId = folderId
            offeredPeerId = peerId
            offeredLabel = label
            offeredRoot = selectedRoot
            offeredPolicy = linkPolicy
            return MUTATION
        }
        override fun accept(connection: NodeConnection, offerId: String, selectedRoot: String): FolderSyncMutation {
            events += "api-accept"
            if (failAccept) error("network failure")
            return MUTATION
        }
        override fun settings(
            connection: NodeConnection,
            folderId: String,
            changeId: String,
            expectedRevision: Long,
            settings: FolderLinkSettings,
        ) = MUTATION
        override fun pause(connection: NodeConnection, offerId: String, paused: Boolean) = MUTATION
        override fun renew(connection: NodeConnection, offerId: String): FolderSyncMutation {
            events += "api-renew"
            return mutation(responseOffer)
        }
        override fun remove(connection: NodeConnection, offerId: String): FolderSyncMutation {
            events += "api-remove"
            return mutation(responseOffer)
        }
        override fun repair(connection: NodeConnection, offerId: String, selectedRoot: String): FolderSyncMutation {
            events += "api-repair"
            if (failRepair) error("network failure")
            return mutation(responseOffer)
        }
        override fun retry(connection: NodeConnection) = MUTATION
        override fun refreshPeerAddress(
            connection: NodeConnection,
            request: PeerAddressRefreshRequest,
        ): FolderSyncMutation {
            events += "api-refresh-address"
            addressRequest = request
            return mutation(addressResponseOffer)
        }
    }

    private companion object {
        val CONNECTION = NodeConnection("http://127.0.0.1:8787", "token")
        const val OFFER = "33333333-3333-4333-8333-333333333333"
        const val OTHER_OFFER = "44444444-4444-4444-8444-444444444444"
        const val SOURCE_FOLDER = "11111111-1111-4111-8111-111111111111"
        const val SOURCE_PEER = "22222222-2222-4222-8222-222222222222"
        fun mutation(offerId: String?) = FolderSyncMutation(
            offerId,
            FolderSyncLifecycle.STOPPED,
            null,
        )
        val MUTATION = FolderSyncMutation(
            OFFER,
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
