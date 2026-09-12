package life.michaelwong.covalent.sync

import android.Manifest
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.CancellationSignal
import android.provider.DocumentsContract
import androidx.core.net.toUri
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

class SafStrictQueryInstrumentedTest {
    private val instrumentation = InstrumentationRegistry.getInstrumentation()
    private val targetContext = instrumentation.targetContext
    private val grantedUris = mutableListOf<Uri>()

    @Before
    fun resetProvider() {
        setMode(SyncTestDocumentsProvider.MODE_STABLE)
    }

    @After
    fun revokeGrants() {
        withProviderControl {
            grantedUris.forEach { uri ->
                targetContext.revokeUriPermission(targetContext.packageName, uri, ACCESS_FLAGS)
            }
        }
        grantedUris.clear()
    }

    @Test
    fun realResolverStableScanPreservesOpaqueIdsWithoutOpeningContent() {
        val observation = SafStrictQuery.observeMetadata(
            targetContext.contentResolver,
            grantedTree(),
        )

        assertEquals(SyncTestDocumentsProvider.ROOT_ID, observation.rootDocumentId)
        assertEquals(
            listOf(
                SyncTestDocumentsProvider.DIRECTORY_ID,
                SyncTestDocumentsProvider.ROOT_FILE_ID,
                SyncTestDocumentsProvider.NESTED_FILE_ID,
            ),
            observation.entries.map { it.documentId },
        )
        assertEquals(listOf(1, 1, 2), observation.entries.map { it.depth })
        assertTrue(observation.toString().contains("contentVerified=false"))
        assertFalse(observation.toString().contains("top.txt"))
    }

    @Test
    fun realResolverNullLoadingAndErrorCursorsFailClosed() {
        val tree = grantedTree()
        listOf(
            SyncTestDocumentsProvider.MODE_NULL to SafSyncInventoryFailure.NULL_CURSOR,
            SyncTestDocumentsProvider.MODE_LOADING to SafSyncInventoryFailure.LOADING_CURSOR,
            SyncTestDocumentsProvider.MODE_ERROR to SafSyncInventoryFailure.ERROR_CURSOR,
        ).forEach { (mode, reason) ->
            setMode(mode)
            assertFailure(reason) { SafStrictQuery.observeMetadata(targetContext.contentResolver, tree) }
        }
    }

    @Test
    fun realResolverFailureAfterOneCursorRowNeverReturnsPartialMetadata() {
        setMode(SyncTestDocumentsProvider.MODE_THROW_AFTER_ROW)

        assertFailure(SafSyncInventoryFailure.PROVIDER_QUERY_FAILED) {
            SafStrictQuery.observeMetadata(targetContext.contentResolver, grantedTree())
        }
    }

    @Test
    fun realResolverPartialDocumentAndMutationAreRejected() {
        val tree = grantedTree()
        setMode(SyncTestDocumentsProvider.MODE_PARTIAL)
        assertFailure(SafSyncInventoryFailure.PARTIAL_DOCUMENT) {
            SafStrictQuery.observeMetadata(targetContext.contentResolver, tree)
        }
        setMode(SyncTestDocumentsProvider.MODE_MUTATE)
        assertFailure(SafSyncInventoryFailure.UNSTABLE_OBSERVATION) {
            SafStrictQuery.observeMetadata(targetContext.contentResolver, tree)
        }
    }

    @Test
    fun realResolverCyclesDuplicatesAndPreliminaryNameCollisionsAreVisible() {
        val tree = grantedTree()
        listOf(
            SyncTestDocumentsProvider.MODE_CYCLE to SafSyncInventoryFailure.DOCUMENT_CYCLE,
            SyncTestDocumentsProvider.MODE_DUPLICATE to SafSyncInventoryFailure.DUPLICATE_DOCUMENT_ID,
            SyncTestDocumentsProvider.MODE_NAME_COLLISION to SafSyncInventoryFailure.PORTABLE_NAME_COLLISION,
        ).forEach { (mode, reason) ->
            setMode(mode)
            assertFailure(reason) { SafStrictQuery.observeMetadata(targetContext.contentResolver, tree) }
        }
    }

    @Test
    fun realResolverCancellationAndEntryLimitStopWithoutAnObservation() {
        val tree = grantedTree()
        val cancelled = CancellationSignal().apply { cancel() }
        assertFailure(SafSyncInventoryFailure.CANCELLED) {
            SafStrictQuery.observeMetadata(targetContext.contentResolver, tree, cancelled)
        }
        assertFailure(SafSyncInventoryFailure.ENTRY_LIMIT) {
            SafStrictQuery.observeMetadata(
                targetContext.contentResolver,
                tree,
                limits = SafSyncInventoryLimits(maxEntries = 1),
            )
        }
    }

    private fun assertFailure(reason: SafSyncInventoryFailure, action: () -> Unit) {
        val failure = assertThrows(SafSyncInventoryException::class.java) { action() }
        assertEquals(reason, failure.reason)
    }

    private fun setMode(mode: String) {
        withProviderControl {
            targetContext.contentResolver.call(
                FIXTURE_URI,
                SyncTestDocumentsProvider.METHOD_SET_MODE,
                mode,
                null,
            )
        }
    }

    private fun grantedTree(): Uri {
        val uri = DocumentsContract.buildTreeDocumentUri(
            SyncTestDocumentsProvider.AUTHORITY,
            SyncTestDocumentsProvider.ROOT_ID,
        )
        withProviderControl {
            targetContext.grantUriPermission(targetContext.packageName, uri, ACCESS_FLAGS)
        }
        grantedUris += uri
        return uri
    }

    private inline fun <T> withProviderControl(action: () -> T): T {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            instrumentation.uiAutomation.adoptShellPermissionIdentity(Manifest.permission.MANAGE_DOCUMENTS)
        } else {
            instrumentation.uiAutomation.adoptShellPermissionIdentity()
        }
        return try {
            action()
        } finally {
            instrumentation.uiAutomation.dropShellPermissionIdentity()
        }
    }

    private companion object {
        val FIXTURE_URI: Uri = "content://${SyncTestDocumentsProvider.AUTHORITY}".toUri()
        const val ACCESS_FLAGS: Int =
            Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_PREFIX_URI_PERMISSION
    }
}
