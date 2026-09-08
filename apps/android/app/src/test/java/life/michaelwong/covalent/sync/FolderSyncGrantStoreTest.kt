package life.michaelwong.covalent.sync

import java.util.UUID
import life.michaelwong.covalent.model.FolderShare
import life.michaelwong.covalent.model.FolderSharePhase
import life.michaelwong.covalent.model.FolderSyncStatus
import life.michaelwong.covalent.model.FolderSyncAvailability
import life.michaelwong.covalent.model.FolderSyncLifecycle
import life.michaelwong.covalent.model.FolderHealthFreshness
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class FolderSyncGrantStoreTest {
    @Test
    fun repairKeepsAcknowledgedRootUntilAckAndExactPendingChoiceSurvivesReopen() {
        val disk = MemoryPersistence()
        val store = FolderSyncGrantStore(disk)
        store.prepareAcceptance(OFFER, OLD_ROOT)
        store.prepareRepair(OFFER, NEW_ROOT)

        val pending = FolderSyncGrantStore(disk).records().single()
        assertTrue(FolderSyncGrantStore(disk).hasPendingCapabilityChange())
        assertEquals(OLD_ROOT, pending.root)
        assertEquals(NEW_ROOT, pending.pendingRoot)
        assertFalse(pending.toString().contains(OLD_ROOT))
        assertFalse(pending.toString().contains(NEW_ROOT))
        assertThrows(IllegalStateException::class.java) {
            FolderSyncGrantStore(disk).prepareRepair(OFFER, OTHER_ROOT)
        }

        FolderSyncGrantStore(disk).finishRepair(OFFER, NEW_ROOT)
        val acknowledged = FolderSyncGrantStore(disk).records().single()
        assertEquals(NEW_ROOT, acknowledged.root)
        assertNull(acknowledged.pendingRoot)
        assertFalse(FolderSyncGrantStore(disk).hasPendingCapabilityChange())
    }

    @Test
    fun removalTombstoneSurvivesReopenAndClearsTheCapabilityOnlyAfterAck() {
        val disk = MemoryPersistence()
        FolderSyncGrantStore(disk).prepareAcceptance(OFFER, OLD_ROOT)
        FolderSyncGrantStore(disk).prepareRemoval(OFFER)

        val pending = FolderSyncGrantStore(disk).records().single()
        assertTrue(FolderSyncGrantStore(disk).hasPendingCapabilityChange())
        assertTrue(pending.pendingRemoval)
        assertEquals(OLD_ROOT, pending.root)

        FolderSyncGrantStore(disk).finishRemoval(OFFER)
        assertTrue(FolderSyncGrantStore(disk).records().isEmpty())
        assertFalse(FolderSyncGrantStore(disk).hasPendingCapabilityChange())
        // An exact retry after the local acknowledgement is already durable is harmless.
        FolderSyncGrantStore(disk).finishRemoval(OFFER)
    }

    @Test
    fun failedDurablePrepareLeavesThePreviousCapabilityBytesUnchanged() {
        val disk = MemoryPersistence()
        FolderSyncGrantStore(disk).prepareAcceptance(OFFER, OLD_ROOT)
        val before = disk.value
        disk.failWrites = true

        assertThrows(IllegalStateException::class.java) {
            FolderSyncGrantStore(disk).prepareRepair(OFFER, NEW_ROOT)
        }
        assertEquals(before, disk.value)
        val reopened = FolderSyncGrantStore(disk).records().single()
        assertEquals(OLD_ROOT, reopened.root)
        assertNull(reopened.pendingRoot)
    }

    @Test
    fun unacceptedInvitationRemovalDoesNotRequireOrInventAFolderCapability() {
        val disk = MemoryPersistence()
        val store = FolderSyncGrantStore(disk)
        store.prepareRemoval(OFFER)
        store.finishRemoval(OFFER)
        store.prepareRemoval(OFFER)
        assertTrue(store.records().isEmpty())
        assertFalse(store.hasPendingCapabilityChange())
        assertNull(disk.value)
    }

    @Test
    fun acceptanceRetryCannotOverwriteOriginalChoiceOrPendingRepairOrRemoval() {
        val disk = MemoryPersistence()
        val store = FolderSyncGrantStore(disk)
        store.prepareAcceptance(OFFER, OLD_ROOT)
        val accepted = disk.value
        store.prepareAcceptance(OFFER, OLD_ROOT)
        assertEquals(accepted, disk.value)
        assertThrows(IllegalStateException::class.java) { store.prepareAcceptance(OFFER, NEW_ROOT) }
        assertEquals(accepted, disk.value)

        store.prepareRepair(OFFER, NEW_ROOT)
        val repair = disk.value
        assertThrows(IllegalStateException::class.java) { store.prepareAcceptance(OFFER, OLD_ROOT) }
        assertEquals(repair, disk.value)

        store.prepareRemoval(OFFER)
        val removal = disk.value
        assertThrows(IllegalStateException::class.java) { store.prepareAcceptance(OFFER, OLD_ROOT) }
        assertEquals(removal, disk.value)
    }

    @Test
    fun versionOneChoicesRemainReadableAndUpgradeOnlyOnNextDurableMutation() {
        val disk = MemoryPersistence(V1_ACCEPTANCE)
        val old = FolderSyncGrantStore(disk).records().single()
        assertEquals(OLD_ROOT, old.root)
        assertNull(old.pendingRoot)
        assertFalse(old.pendingRemoval)
        assertEquals(V1_ACCEPTANCE, disk.value)

        FolderSyncGrantStore(disk).prepareRepair(OFFER, NEW_ROOT)
        assertTrue(checkNotNull(disk.value).contains("\"schemaVersion\":2"))
        assertEquals(NEW_ROOT, FolderSyncGrantStore(disk).pendingRepairRoot(OFFER))
    }

    @Test
    fun renewedSenderBindingSurvivesReopenAndFailedSaveRetainsExactOriginal() {
        val disk = MemoryPersistence()
        val store = FolderSyncGrantStore(disk)
        store.prepareOffer(PEER, UUID.fromString(FOLDER), OLD_ROOT, "Photos")
        store.finishOffer(UUID.fromString(FOLDER), OFFER)
        val before = disk.value
        val replacement = renewalShare(incoming = false)
        disk.failWrites = true
        assertThrows(IllegalStateException::class.java) { store.reconcile(renewalStatus(replacement)) }
        assertEquals(before, disk.value)
        disk.failWrites = false
        FolderSyncGrantStore(disk).reconcile(renewalStatus(replacement))
        val reopened = FolderSyncGrantStore(disk).records().single()
        assertEquals(REPLACEMENT, reopened.offerId)
        assertEquals(OLD_ROOT, reopened.root)
        assertEquals(FOLDER, reopened.folderId)
        assertEquals(PEER, reopened.peerId)
        assertFalse(FolderSyncGrantStore(disk).hasPendingCapabilityChange())
    }

    @Test
    fun recipientRenewalRetiresOldChoiceAndRequiresExplicitAcceptanceAgain() {
        val disk = MemoryPersistence()
        val store = FolderSyncGrantStore(disk)
        store.prepareAcceptance(OFFER, OLD_ROOT)
        store.reconcile(renewalStatus(renewalShare(incoming = true)))
        assertTrue(FolderSyncGrantStore(disk).records().isEmpty())
        FolderSyncGrantStore(disk).prepareAcceptance(REPLACEMENT, NEW_ROOT)
        assertEquals(NEW_ROOT, FolderSyncGrantStore(disk).records().single().root)
        assertEquals(REPLACEMENT, FolderSyncGrantStore(disk).records().single().offerId)
    }

    @Test
    fun absentRowsNeverDropChoicesAndAmbiguousRenewalsNeverWrite() {
        val disk = MemoryPersistence()
        val store = FolderSyncGrantStore(disk)
        store.prepareAcceptance(OFFER, OLD_ROOT)
        val before = disk.value
        store.reconcile(renewalStatus())
        assertEquals(before, disk.value)
        val replacement = renewalShare(incoming = true)
        assertThrows(IllegalStateException::class.java) {
            store.reconcile(renewalStatus(replacement, replacement.copy(offerId = FOLDER)))
        }
        assertEquals(before, disk.value)
    }

    @Test
    fun removalAfterMissedRenewalRetiresEitherSideAndRetriesFailedSave() {
        for (incoming in listOf(false, true)) {
            val disk = MemoryPersistence()
            val store = FolderSyncGrantStore(disk)
            if (incoming) store.prepareAcceptance(OFFER, OLD_ROOT)
            else {
                store.prepareOffer(PEER, UUID.fromString(FOLDER), OLD_ROOT, "Photos")
                store.finishOffer(UUID.fromString(FOLDER), OFFER)
            }
            val before = disk.value
            val removed = renewalShare(incoming).copy(phase = FolderSharePhase.REMOVED, remoteRemovalPending = true)
            disk.failWrites = true
            assertThrows(IllegalStateException::class.java) { store.reconcile(renewalStatus(removed)) }
            assertEquals(before, disk.value)
            disk.failWrites = false
            FolderSyncGrantStore(disk).reconcile(renewalStatus(removed))
            assertTrue(FolderSyncGrantStore(disk).records().isEmpty())
        }
    }

    @Test
    fun pendingOfferRemovalRequiresAnExactPeerFolderAndLabel() {
        val disk = MemoryPersistence()
        val store = FolderSyncGrantStore(disk)
        store.prepareOffer(PEER, UUID.fromString(FOLDER), OLD_ROOT, "Photos")
        val removed = renewalShare(false).copy(phase = FolderSharePhase.REMOVED)
        val before = disk.value
        for (foreign in listOf(removed.copy(peerId = OFFER), removed.copy(label = "Other"), removed.copy(folderId = OFFER))) {
            store.reconcile(renewalStatus(foreign))
            assertEquals(before, disk.value)
        }
        store.reconcile(renewalStatus(removed))
        assertTrue(FolderSyncGrantStore(disk).records().isEmpty())
    }

    @Test
    fun pendingOfferNeverRebindsToAnOfferIdAlreadyClaimedByAnotherGrant() {
        val disk = MemoryPersistence()
        val store = FolderSyncGrantStore(disk)
        store.prepareOffer(PEER, UUID.fromString(FOLDER), OLD_ROOT, "Photos")
        store.finishOffer(UUID.fromString(FOLDER), OFFER)
        store.prepareOffer(PEER, UUID.fromString(FOLDER), OLD_ROOT, "Photos")
        val before = disk.value
        val existing = renewalShare(false).copy(offerId = OFFER, supersededOfferIds = emptyList())

        store.reconcile(renewalStatus(existing))

        assertEquals(before, disk.value)
        assertEquals(setOf("offer:$OFFER", "folder:$FOLDER"), store.records().map { it.key }.toSet())
    }

    private fun renewalShare(incoming: Boolean) = FolderShare(
        REPLACEMENT, FOLDER, "Photos", PEER, incoming, FolderSharePhase.OFFERED,
        100, false, supersededOfferIds = listOf(OFFER),
    )

    private fun renewalStatus(vararg shares: FolderShare) = FolderSyncStatus(
        FolderSyncAvailability.AVAILABLE, FolderSyncLifecycle.RUNNING, null,
        FolderHealthFreshness.FRESH, emptyList(), shares.toList(), emptyList(),
    )

    private class MemoryPersistence(initial: String? = null) : FolderSyncGrantPersistence {
        override val readable = true
        var value: String? = initial
        var failWrites = false

        override fun read(): String? = value
        override fun write(value: String): Boolean {
            if (failWrites) return false
            this.value = value
            return true
        }
    }

    private companion object {
        const val OFFER = "33333333-3333-4333-8333-333333333333"
        const val REPLACEMENT = "44444444-4444-4444-8444-444444444444"
        const val FOLDER = "11111111-1111-4111-8111-111111111111"
        const val PEER = "22222222-2222-4222-8222-222222222222"
        const val OLD_ROOT = "/storage/emulated/0/Photos"
        const val NEW_ROOT = "/storage/emulated/0/Repaired"
        const val OTHER_ROOT = "/storage/emulated/0/Other"
        const val V1_ACCEPTANCE = """[{"schemaVersion":1,"key":"offer:$OFFER","kind":"acceptance","offerId":"$OFFER","folderId":null,"peerId":null,"root":"$OLD_ROOT","label":null}]"""
    }
}
