package life.michaelwong.covalent

import android.content.Intent
import android.net.Uri
import android.provider.DocumentsContract
import androidx.core.net.toUri
import androidx.documentfile.provider.DocumentFile
import androidx.test.platform.app.InstrumentationRegistry
import life.michaelwong.covalent.data.SafSourceAccessException
import life.michaelwong.covalent.data.SafTargetAccessException
import life.michaelwong.covalent.data.SafTransferBridge
import life.michaelwong.covalent.data.TestDocumentsProvider
import org.json.JSONObject
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

class SafDocumentsProviderTest {
    private val instrumentation = InstrumentationRegistry.getInstrumentation()
    private val fixtureContext = instrumentation.context
    private val targetContext = instrumentation.targetContext
    private val grantedUris = mutableListOf<Uri>()

    @Before
    fun resetFixture() {
        setMode(TestDocumentsProvider.MODE_STABLE)
    }

    @After
    fun revokeFixtureGrants() {
        grantedUris.forEach { uri ->
            fixtureContext.revokeUriPermission(targetContext.packageName, uri, ACCESS_FLAGS)
        }
        grantedUris.clear()
    }

    @Test
    fun childDocumentUriRemainsChildAndInventoryStaysBelowIt() {
        val rootTree = grantedTree(TestDocumentsProvider.ROOT_ID)
        val childDocumentUri = DocumentsContract.buildDocumentUriUsingTree(
            rootTree,
            TestDocumentsProvider.CHILD_ID,
        )
        val document = requireNotNull(DocumentFile.fromTreeUri(targetContext, childDocumentUri))

        assertEquals(
            TestDocumentsProvider.CHILD_ID,
            DocumentsContract.getDocumentId(document.uri),
        )
        val rootDocumentUri = DocumentsContract.buildDocumentUriUsingTree(
            rootTree,
            TestDocumentsProvider.ROOT_ID,
        )
        assertTrue(
            DocumentsContract.isChildDocument(
                targetContext.contentResolver,
                rootDocumentUri,
                childDocumentUri,
            ),
        )
        val rootSiblingUri = DocumentsContract.buildDocumentUriUsingTree(rootTree, "root/top.txt")
        assertFalse(
            DocumentsContract.isChildDocument(
                targetContext.contentResolver,
                childDocumentUri,
                rootSiblingUri,
            ),
        )
        assertEquals(
            listOf("inside.txt"),
            SafTransferBridge().targetInventory(targetContext, childDocumentUri).entries.map { it.path },
        )
    }

    @Test
    fun nullSourceChildQueryFailsBeforeAnEmptyBackupCanBeUploaded() {
        setMode(TestDocumentsProvider.MODE_NULL_CHILD_QUERY)
        val rootTree = grantedTree(TestDocumentsProvider.ROOT_ID)

        val failure = assertThrows(SafSourceAccessException::class.java) {
            SafTransferBridge().createBackup(
                targetContext,
                "https://127.0.0.1:1",
                "unused-test-token",
                rootTree,
                JSONObject(),
            ) { _, _ -> }
        }
        assertNull(failure.cause)
    }

    @Test
    fun thrownTargetChildQueryIsALocalTargetFailure() {
        setMode(TestDocumentsProvider.MODE_THROW_CHILD_QUERY)
        val rootTree = grantedTree(TestDocumentsProvider.ROOT_ID)

        val failure = assertThrows(SafTargetAccessException::class.java) {
            SafTransferBridge().targetInventory(targetContext, rootTree)
        }
        assertTrue(
            generateSequence<Throwable>(failure) { it.cause }
                .any { it.message?.contains("Synthetic child query failure") == true },
        )
    }

    @Test
    fun mutationBetweenInventoryQueriesIsRejected() {
        setMode(TestDocumentsProvider.MODE_MUTATE_CHILD_QUERY)
        val rootTree = grantedTree(TestDocumentsProvider.ROOT_ID)

        assertThrows(SafTargetAccessException::class.java) {
            SafTransferBridge().targetInventory(targetContext, rootTree)
        }
    }

    @Test
    fun mutationBetweenArchiveQueriesIsRejectedBeforeUpload() {
        setMode(TestDocumentsProvider.MODE_MUTATE_CHILD_QUERY)
        val rootTree = grantedTree(TestDocumentsProvider.ROOT_ID)

        assertThrows(SafSourceAccessException::class.java) {
            SafTransferBridge().createBackup(
                targetContext,
                "https://127.0.0.1:1",
                "unused-test-token",
                rootTree,
                JSONObject(),
            ) { _, _ -> }
        }
    }

    private fun setMode(mode: String) {
        fixtureContext.contentResolver.call(FIXTURE_URI, TestDocumentsProvider.METHOD_SET_MODE, mode, null)
    }

    private fun grantedTree(documentId: String): Uri {
        val uri = DocumentsContract.buildTreeDocumentUri(TestDocumentsProvider.AUTHORITY, documentId)
        fixtureContext.grantUriPermission(targetContext.packageName, uri, ACCESS_FLAGS)
        grantedUris += uri
        return uri
    }

    private companion object {
        val FIXTURE_URI: Uri = "content://${TestDocumentsProvider.AUTHORITY}".toUri()
        const val ACCESS_FLAGS: Int =
            Intent.FLAG_GRANT_READ_URI_PERMISSION or
                Intent.FLAG_GRANT_WRITE_URI_PERMISSION or
                Intent.FLAG_GRANT_PREFIX_URI_PERMISSION
    }
}
