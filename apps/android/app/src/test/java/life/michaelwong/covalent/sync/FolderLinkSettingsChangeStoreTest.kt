package life.michaelwong.covalent.sync

import life.michaelwong.covalent.model.FolderHealthFreshness
import life.michaelwong.covalent.model.FolderLinkPolicy
import life.michaelwong.covalent.model.FolderLinkSettings
import life.michaelwong.covalent.model.FolderLinkSettingsChange
import life.michaelwong.covalent.model.FolderLinkSettingsState
import life.michaelwong.covalent.model.FolderShare
import life.michaelwong.covalent.model.FolderSharePhase
import life.michaelwong.covalent.model.FolderSyncAvailability
import life.michaelwong.covalent.model.FolderSyncLifecycle
import life.michaelwong.covalent.model.FolderSyncStatus
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class FolderLinkSettingsChangeStoreTest {
    @Test
    fun exactUncertainRequestSurvivesReopenAndReusesItsIdentity() {
        val disk = MemoryPersistence()
        val first = FolderLinkSettingsChangeStore(disk).prepare(FOLDER, 4, SETTINGS)
        val reopened = FolderLinkSettingsChangeStore(disk)

        assertEquals(first, reopened.records().single())
        assertEquals(first, reopened.prepare(FOLDER, 4, SETTINGS))
        assertNotEquals(NIL, first.changeId)
    }

    @Test
    fun anotherSubmitIsBlockedUntilTheSavedAttemptIsReviewed() {
        val store = FolderLinkSettingsChangeStore(MemoryPersistence())
        val saved = store.prepare(FOLDER, 4, SETTINGS)
        assertThrows(IllegalStateException::class.java) {
            store.prepare(FOLDER, 4, SETTINGS.copy(paused = true))
        }

        store.markForReview(FOLDER, saved.changeId)
        assertTrue(store.records().single().requiresReview)
        assertEquals(saved.copy(requiresReview = true), store.takeForReview(FOLDER))
        assertTrue(store.records().isEmpty())
    }

    @Test
    fun statusClearsObservedRequestsAndMarksUnobservedStaleRequestsForReview() {
        val disk = MemoryPersistence()
        val store = FolderLinkSettingsChangeStore(disk)
        val pending = store.prepare(FOLDER, 4, SETTINGS)
        store.reconcile(status(4, pendingChange = pending.toWire()))
        assertTrue(store.records().isEmpty())

        val stale = store.prepare(FOLDER, 4, SETTINGS)
        store.reconcile(status(5))
        assertEquals(stale.copy(requiresReview = true), store.records().single())
    }

    @Test
    fun removingTheLinkRemovesItsSavedRequest() {
        val store = FolderLinkSettingsChangeStore(MemoryPersistence())
        store.prepare(FOLDER, 4, SETTINGS)
        store.reconcile(status(4, phase = FolderSharePhase.REMOVED))
        assertTrue(store.records().isEmpty())
    }

    private fun SavedFolderLinkSettingsChange.toWire() = FolderLinkSettingsChange(
        FOLDER,
        SOURCE,
        REQUESTER,
        changeId,
        expectedRevision,
        settings,
    )

    private fun status(
        revision: Long,
        pendingChange: FolderLinkSettingsChange? = null,
        phase: FolderSharePhase = FolderSharePhase.READY,
    ) = FolderSyncStatus(
        FolderSyncAvailability.AVAILABLE,
        FolderSyncLifecycle.RUNNING,
        null,
        FolderHealthFreshness.FRESH,
        emptyList(),
        listOf(FolderShare(
            OFFER,
            FOLDER,
            "Photos",
            SOURCE,
            false,
            phase,
            null,
            false,
            linkPolicy = POLICY,
            linkSettings = FolderLinkSettingsState(
                revision,
                SETTINGS,
                if (revision == 0L) NIL else COMMIT,
                SOURCE,
                true,
                pendingChange,
                null,
            ),
        )),
        emptyList(),
    )

    private class MemoryPersistence : FolderLinkSettingsChangePersistence {
        override val readable = true
        var value: String? = null
        override fun read() = value
        override fun write(value: String): Boolean {
            this.value = value
            return true
        }
    }

    private companion object {
        const val FOLDER = "11111111-1111-4111-8111-111111111111"
        const val SOURCE = "22222222-2222-4222-8222-222222222222"
        const val OFFER = "33333333-3333-4333-8333-333333333333"
        const val REQUESTER = "44444444-4444-4444-8444-444444444444"
        const val COMMIT = "55555555-5555-4555-8555-555555555555"
        const val NIL = "00000000-0000-0000-0000-000000000000"
        val POLICY = FolderLinkPolicy(false, true)
        val SETTINGS = FolderLinkSettings(POLICY, paused = false)
    }
}
