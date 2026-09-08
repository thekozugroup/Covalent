package life.michaelwong.covalent.sync

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
        const val OLD_ROOT = "/storage/emulated/0/Photos"
        const val NEW_ROOT = "/storage/emulated/0/Repaired"
        const val OTHER_ROOT = "/storage/emulated/0/Other"
        const val V1_ACCEPTANCE = """[{"schemaVersion":1,"key":"offer:$OFFER","kind":"acceptance","offerId":"$OFFER","folderId":null,"peerId":null,"root":"$OLD_ROOT","label":null}]"""
    }
}
