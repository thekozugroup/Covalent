package life.michaelwong.covalent.sync

import android.Manifest
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.provider.DocumentsContract
import android.util.Base64
import androidx.core.net.toUri
import androidx.documentfile.provider.DocumentFile
import androidx.test.platform.app.InstrumentationRegistry
import java.io.ByteArrayOutputStream
import java.net.Socket
import org.junit.After
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.json.JSONArray

class SafWebDavServerInstrumentedTest {
    private val instrumentation = InstrumentationRegistry.getInstrumentation()
    private val context = instrumentation.targetContext
    private val treeUri = DocumentsContract.buildTreeDocumentUri(
        SafWebDavTestDocumentsProvider.AUTHORITY,
        SafWebDavTestDocumentsProvider.ROOT_ID,
    )
    private var server: SafWebDavServer? = null

    @Before
    fun resetAndGrant() {
        withProviderControl {
            context.contentResolver.call(
                "content://${SafWebDavTestDocumentsProvider.AUTHORITY}".toUri(),
                SafWebDavTestDocumentsProvider.METHOD_RESET,
                null,
                null,
            )
            context.grantUriPermission(context.packageName, treeUri, ACCESS_FLAGS)
        }
    }

    @After
    fun closeAndRevoke() {
        server?.close()
        withProviderControl { context.revokeUriPermission(context.packageName, treeUri, ACCESS_FLAGS) }
    }

    @Test
    fun authenticatedAdapterStreamsOneTreeAndRejectsEscapes() {
        val endpoint = start()
        assertFalse(endpoint.toString().contains(USERNAME))
        assertFalse(endpoint.toString().contains(PASSWORD))
        assertEquals(401, request(endpoint, "PROPFIND", "/", authenticated = false).code)

        val listing = request(endpoint, "PROPFIND", "/", headers = mapOf("Depth" to "1"))
        assertEquals(207, listing.code)
        assertTrue(listing.body.decodeToString().contains("Gr%C3%BC%C3%9Fe.txt"))
        assertFalse("The listing must not contain file bytes", listing.body.decodeToString().contains("hello"))

        val content = buildString {
            repeat(16_384) { append("Grüße line $it\n") }
        }.encodeToByteArray()
        assertEquals(201, request(endpoint, "PUT", "/Folder.txt", content).code)
        assertArrayEquals(content, request(endpoint, "GET", "/Folder.txt").body)
        assertEquals(200, request(endpoint, "HEAD", "/Folder.txt").code)

        assertEquals(201, request(endpoint, "MKCOL", "/Nested").code)
        assertEquals(201, request(endpoint, "MOVE", "/Folder.txt", headers = mapOf(
            "Destination" to "http://127.0.0.1:${endpoint.port}/Nested/Moved.txt",
        )).code)
        assertEquals(404, request(endpoint, "GET", "/Folder.txt").code)
        assertArrayEquals(content, request(endpoint, "GET", "/Nested/Moved.txt").body)
        assertEquals(204, request(endpoint, "DELETE", "/Nested/Moved.txt").code)

        assertEquals(400, request(endpoint, "GET", "/%2e%2e/secret").code)
        assertEquals(400, request(endpoint, "GET", "/encoded%2Fslash").code)
        assertEquals(400, request(endpoint, "MOVE", "/Gr%C3%BC%C3%9Fe.txt", headers = mapOf(
            "Destination" to "http://127.0.0.1:${endpoint.port + 1}/escaped.txt",
        )).code)
        assertEquals(404, request(endpoint, "GET", "/escaped.txt").code)
    }

    @Test
    fun interruptedNewUploadIsRemovedAndGrantSurvivesServerRestart() {
        var endpoint = start()
        Socket("127.0.0.1", endpoint.port).use { socket ->
            val body = byteArrayOf(1, 2, 3, 4)
            val head = requestHead(
                endpoint,
                "PUT",
                "/partial.bin",
                mapOf("Content-Length" to "4096", "Content-Type" to "application/octet-stream"),
            )
            socket.getOutputStream().write(head)
            socket.getOutputStream().write(body)
            socket.getOutputStream().flush()
            socket.shutdownOutput()
        }
        repeat(50) {
            if (request(endpoint, "GET", "/partial.bin").code == 404) return@repeat
            Thread.sleep(20)
        }
        assertEquals(404, request(endpoint, "GET", "/partial.bin").code)

        val expected = "persisted across adapter restart\n".encodeToByteArray()
        assertEquals(201, request(endpoint, "PUT", "/restart.txt", expected).code)
        server?.close()
        server = null
        endpoint = start()
        assertArrayEquals(expected, request(endpoint, "GET", "/restart.txt").body)
    }

    @Test
    fun grantStorePersistsOnlyOpaqueTokensAndFailsClosedWhenPermissionIsLost() {
        val persistence = MemoryPersistence()
        var retained = false
        val store = SafFolderGrantStore(
            persistence,
            persistedPermissions = { listOf(treeUri to retained) },
            takePermission = { selected ->
                assertEquals(treeUri, selected)
                retained = true
            },
        )
        val grant = store.persist(treeUri)
        assertEquals(grant, store.persist(treeUri))
        assertEquals(grant.selectedRoot, store.requireSelectedRoot(grant.selectedRoot))
        assertEquals(treeUri.toString(), JSONArray(persistence.value).getJSONObject(0).getString("treeUri"))
        assertFalse(grant.toString().contains(treeUri.toString()))

        val reopened = SafFolderGrantStore(
            persistence,
            persistedPermissions = { listOf(treeUri to retained) },
            takePermission = { error("No new permission should be requested") },
        )
        assertEquals(grant.selectedRoot, reopened.requireSelectedRoot(grant.selectedRoot))
        retained = false
        org.junit.Assert.assertThrows(IllegalStateException::class.java) {
            reopened.requireSelectedRoot(grant.selectedRoot)
        }
        org.junit.Assert.assertThrows(IllegalArgumentException::class.java) {
            reopened.requireSelectedRoot("/storage/emulated/0/Photos")
        }
        org.junit.Assert.assertThrows(IllegalArgumentException::class.java) {
            reopened.requireSelectedRoot(grant.selectedRoot.uppercase())
        }
    }

    @Test
    fun restrictedRcloneCanStreamIntoTheSelectedSafTree() {
        val endpoint = start()
        println("COVALENT_SAF_TEST_PORT=${endpoint.port}")
        val deadline = System.nanoTime() + 45_000_000_000L
        var uploaded: DocumentFile? = null
        var roundTrip: DocumentFile? = null
        while (System.nanoTime() < deadline && (uploaded == null || roundTrip == null)) {
            val root = DocumentFile.fromTreeUri(context, treeUri)
            uploaded = root?.findFile("From-rclone.txt")
            roundTrip = root?.findFile("Roundtrip.txt")
            if (uploaded == null || roundTrip == null) Thread.sleep(100)
        }
        val uri = requireNotNull(uploaded?.uri) { "The restricted rclone worker did not upload the test file." }
        val actual = context.contentResolver.openInputStream(uri)?.use { it.readBytes() }
        assertArrayEquals(RCLONE_CONTENT, requireNotNull(actual))
        val roundTripUri = requireNotNull(roundTrip?.uri) { "The restricted rclone worker did not finish the read-back." }
        val roundTripBytes = context.contentResolver.openInputStream(roundTripUri)?.use { it.readBytes() }
        assertArrayEquals("hello\nworld\n".encodeToByteArray(), requireNotNull(roundTripBytes))
    }

    private fun start(): SafWebDavEndpoint {
        val instance = SafWebDavServer(context, treeUri, USERNAME, PASSWORD)
        server = instance
        return instance.start()
    }

    private fun request(
        endpoint: SafWebDavEndpoint,
        method: String,
        path: String,
        body: ByteArray = ByteArray(0),
        headers: Map<String, String> = emptyMap(),
        authenticated: Boolean = true,
    ): Response = Socket("127.0.0.1", endpoint.port).use { socket ->
        val requestHeaders = linkedMapOf<String, String>()
        if (body.isNotEmpty() || method == "PUT") requestHeaders["Content-Length"] = body.size.toString()
        requestHeaders.putAll(headers)
        if (authenticated) requestHeaders["Authorization"] = authorization(endpoint)
        val output = socket.getOutputStream()
        output.write(requestHead(endpoint, method, path, requestHeaders, includeAuthorization = false))
        output.write(body)
        output.flush()
        val bytes = socket.getInputStream().readBytes()
        val separator = bytes.indexOfSubsequence("\r\n\r\n".encodeToByteArray())
        check(separator >= 0)
        val head = bytes.copyOfRange(0, separator).decodeToString()
        val code = head.lineSequence().first().split(' ')[1].toInt()
        Response(code, bytes.copyOfRange(separator + 4, bytes.size))
    }

    private fun requestHead(
        endpoint: SafWebDavEndpoint,
        method: String,
        path: String,
        headers: Map<String, String> = emptyMap(),
        includeAuthorization: Boolean = true,
    ): ByteArray = buildString {
        append("$method $path HTTP/1.1\r\n")
        append("Host: 127.0.0.1:${endpoint.port}\r\n")
        append("Connection: close\r\n")
        if (includeAuthorization) append("Authorization: ${authorization(endpoint)}\r\n")
        headers.forEach { (name, value) -> append("$name: $value\r\n") }
        append("\r\n")
    }.encodeToByteArray()

    private fun authorization(endpoint: SafWebDavEndpoint): String = "Basic " + Base64.encodeToString(
        "${endpoint.username}:${endpoint.password}".encodeToByteArray(),
        Base64.NO_WRAP,
    )

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

    private data class Response(val code: Int, val body: ByteArray)

    private class MemoryPersistence : SafFolderGrantPersistence {
        override val readable = true
        var value = "[]"
        override fun read(): String = value
        override fun write(value: String): Boolean {
            this.value = value
            return true
        }
    }

    private companion object {
        const val USERNAME = "covalent-test-user"
        const val PASSWORD = "covalent-test-password-long-enough"
        val RCLONE_CONTENT = "rclone streamed this through the SAF bridge\n".encodeToByteArray()
        const val ACCESS_FLAGS = Intent.FLAG_GRANT_READ_URI_PERMISSION or
            Intent.FLAG_GRANT_WRITE_URI_PERMISSION or Intent.FLAG_GRANT_PREFIX_URI_PERMISSION

        fun ByteArray.indexOfSubsequence(needle: ByteArray): Int {
            for (start in 0..size - needle.size) {
                if (needle.indices.all { this[start + it] == needle[it] }) return start
            }
            return -1
        }
    }
}
