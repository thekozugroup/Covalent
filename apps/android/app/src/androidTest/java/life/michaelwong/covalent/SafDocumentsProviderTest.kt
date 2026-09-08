package life.michaelwong.covalent

import android.Manifest
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.provider.DocumentsContract
import androidx.core.net.toUri
import androidx.documentfile.provider.DocumentFile
import androidx.test.platform.app.InstrumentationRegistry
import java.util.Base64
import life.michaelwong.covalent.data.RecoveryExportMaterial
import life.michaelwong.covalent.data.RecoverySafFiles
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
    private val targetContext = instrumentation.targetContext
    private val grantedUris = mutableListOf<Uri>()

    @Before
    fun resetFixture() {
        setMode(TestDocumentsProvider.MODE_STABLE)
    }

    @After
    fun revokeFixtureGrants() {
        withDocumentProviderControl {
            grantedUris.forEach { uri ->
                targetContext.revokeUriPermission(targetContext.packageName, uri, ACCESS_FLAGS)
            }
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

    @Test
    fun recoveryFilesStayDistinctAndRoundTripThroughTheRealResolver() {
        val kitUri = grantedDocument(TestDocumentsProvider.RECOVERY_KIT_ID)
        val codeUri = grantedDocument(TestDocumentsProvider.RECOVERY_CODE_ID)
        val kit = "encrypted signed recovery kit".encodeToByteArray()
        val rawKey = ByteArray(32) { (it + 1).toByte() }
        val code = Base64.getUrlEncoder().withoutPadding().encode(rawKey)
        val export = RecoveryExportMaterial(kit.copyOf(), code.copyOf())

        export.save(targetContext, kitUri, codeUri)
        assertTrue(export.complete)
        val imported = RecoverySafFiles.readBootstrap(targetContext, kitUri, codeUri)
        assertTrue(imported.kit.contentEquals(kit))
        assertTrue(imported.key.contentEquals(rawKey))
        imported.close()
        export.close()
        rawKey.fill(0)
        code.fill(0)
    }

    @Test
    fun recoveryExportRejectsOneDocumentForBothOutputs() {
        val uri = grantedDocument(TestDocumentsProvider.RECOVERY_KIT_ID)
        val rawKey = ByteArray(32) { it.toByte() }
        val code = Base64.getUrlEncoder().withoutPadding().encode(rawKey)
        val export = RecoveryExportMaterial(byteArrayOf(1, 2, 3), code)
        try {
            assertThrows(IllegalArgumentException::class.java) {
                export.save(targetContext, uri, uri)
            }
        } finally {
            export.close()
            rawKey.fill(0)
        }
    }

    private fun setMode(mode: String) {
        withDocumentProviderControl {
            targetContext.contentResolver.call(
                FIXTURE_URI,
                TestDocumentsProvider.METHOD_SET_MODE,
                mode,
                null,
            )
        }
    }

    private fun grantedTree(documentId: String): Uri {
        val uri = DocumentsContract.buildTreeDocumentUri(TestDocumentsProvider.AUTHORITY, documentId)
        withDocumentProviderControl {
            targetContext.grantUriPermission(targetContext.packageName, uri, ACCESS_FLAGS)
        }
        grantedUris += uri
        return uri
    }

    private fun grantedDocument(documentId: String): Uri {
        val uri = DocumentsContract.buildDocumentUri(TestDocumentsProvider.AUTHORITY, documentId)
        withDocumentProviderControl {
            targetContext.grantUriPermission(targetContext.packageName, uri, ACCESS_FLAGS)
        }
        grantedUris += uri
        return uri
    }

    private inline fun <T> withDocumentProviderControl(action: () -> T): T {
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
        val FIXTURE_URI: Uri = "content://${TestDocumentsProvider.AUTHORITY}".toUri()
        const val ACCESS_FLAGS: Int =
            Intent.FLAG_GRANT_READ_URI_PERMISSION or
                Intent.FLAG_GRANT_WRITE_URI_PERMISSION or
                Intent.FLAG_GRANT_PREFIX_URI_PERMISSION
    }
}
