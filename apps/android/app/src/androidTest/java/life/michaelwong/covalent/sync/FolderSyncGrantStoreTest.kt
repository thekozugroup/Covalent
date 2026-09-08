package life.michaelwong.covalent.sync

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import java.util.Collections
import java.util.UUID
import java.util.concurrent.CountDownLatch
import kotlin.concurrent.thread
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class FolderSyncGrantStoreTest {
    @Test
    fun offerAndAcceptanceRootsAreCommittedBeforeProcessLikeReopenAndStayRedacted() {
        val context = ApplicationProvider.getApplicationContext<Context>()
        val preferences = context.getSharedPreferences("covalent_folder_sync_grants", Context.MODE_PRIVATE)
        val original = preferences.getString("records_v1", null)
        preferences.edit().clear().commit()
        try {
            val folder = UUID.fromString("11111111-1111-4111-8111-111111111111")
            FolderSyncGrantStore(context).prepareOffer(
                "22222222-2222-4222-8222-222222222222",
                folder,
                "/storage/emulated/0/Photos",
                "Photos",
            )
            val reopened = FolderSyncGrantStore(context).records().single()
            assertEquals(folder.toString(), reopened.folderId)
            assertNull(reopened.offerId)
            assertEquals("/storage/emulated/0/Photos", reopened.root)
            assertFalse(reopened.toString().contains("Photos"))

            val offerId = "55555555-5555-4555-8555-555555555555"
            FolderSyncGrantStore(context).finishOffer(folder, offerId)
            val committedOffer = FolderSyncGrantStore(context).records().single()
            assertEquals(offerId, committedOffer.offerId)
            assertEquals(folder.toString(), committedOffer.folderId)
            assertEquals("22222222-2222-4222-8222-222222222222", committedOffer.peerId)

            FolderSyncGrantStore(context).prepareAcceptance(
                "33333333-3333-4333-8333-333333333333",
                "/storage/emulated/0/Shared",
            )
            val reloaded = FolderSyncGrantStore(context).records()
            assertEquals(2, reloaded.size)
            val acceptance = reloaded.single { it.kind == FolderSyncGrantKind.ACCEPTANCE }
            assertNull(acceptance.folderId)
            assertNull(acceptance.peerId)
            assertNull(acceptance.label)
            assertTrue(preferences.contains("records_v1"))

            preferences.edit().putString(
                "records_v1",
                """[{"schemaVersion":1,"key":"offer:33333333-3333-4333-8333-333333333333","kind":"acceptance","offerId":"33333333-3333-4333-8333-333333333333","folderId":null,"peerId":null,"root":"/storage/emulated/0/a/../Shared","label":null}]""",
            ).commit()
            assertThrows(IllegalStateException::class.java) {
                FolderSyncGrantStore(context).records()
            }
        } finally {
            preferences.edit().clear().commit()
            if (original != null) preferences.edit().putString("records_v1", original).commit()
        }
    }

    @Test
    fun concurrentEquivalentOffersReuseOneDurableFolderIdentity() {
        val context = ApplicationProvider.getApplicationContext<Context>()
        val preferences = context.getSharedPreferences("covalent_folder_sync_grants", Context.MODE_PRIVATE)
        val original = preferences.getString("records_v1", null)
        preferences.edit().clear().commit()
        try {
            val start = CountDownLatch(1)
            val results = Collections.synchronizedList(mutableListOf<UUID>())
            val workers = listOf(
                UUID.fromString("11111111-1111-4111-8111-111111111111"),
                UUID.fromString("44444444-4444-4444-8444-444444444444"),
            ).map { proposal ->
                thread {
                    start.await()
                    results.add(FolderSyncGrantStore(context).prepareOffer(
                        "22222222-2222-4222-8222-222222222222",
                        proposal,
                        "/storage/emulated/0/Photos",
                        "Photos",
                    ))
                }
            }
            start.countDown()
            workers.forEach(Thread::join)

            assertEquals(2, results.size)
            assertEquals(1, results.toSet().size)
            assertEquals(1, FolderSyncGrantStore(context).records().size)
        } finally {
            preferences.edit().clear().commit()
            if (original != null) preferences.edit().putString("records_v1", original).commit()
        }
    }
}
