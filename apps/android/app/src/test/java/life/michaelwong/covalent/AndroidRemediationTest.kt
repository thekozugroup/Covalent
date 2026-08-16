package life.michaelwong.covalent

import androidx.lifecycle.SavedStateHandle
import java.util.Base64
import java.security.MessageDigest
import life.michaelwong.covalent.data.CovalentNodeClient
import life.michaelwong.covalent.data.EnrolledTrust
import life.michaelwong.covalent.data.normalizeSha256Pin
import life.michaelwong.covalent.data.restorePlanPageFromPersistence
import life.michaelwong.covalent.data.restoreTransferPayload
import life.michaelwong.covalent.data.toPersistenceJson
import life.michaelwong.covalent.data.requireJobAcknowledgement
import life.michaelwong.covalent.ui.AddressIssue
import life.michaelwong.covalent.ui.CovalentViewModel
import life.michaelwong.covalent.ui.Screen
import life.michaelwong.covalent.ui.isExpired
import life.michaelwong.covalent.ui.normalizeEndpointInput
import life.michaelwong.covalent.ui.requiresLocalNetworkPermission
import life.michaelwong.covalent.ui.settingsPreview
import life.michaelwong.covalent.ui.validateNodeAddress
import life.michaelwong.covalent.ui.validProviderSocketAddress
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import okhttp3.tls.HandshakeCertificates
import okhttp3.tls.HeldCertificate

class AndroidRemediationTest {
    @Test
    fun setupLinkNormalizationAndTransportPolicyAreDeterministic() {
        assertEquals(
            "https://node.example:8443",
            normalizeEndpointInput("covalent://connect?endpoint=https%3A%2F%2Fnode.example%3A8443"),
        )
        assertEquals(AddressIssue.NONE, validateNodeAddress("https://node.example:8443"))
        assertEquals(AddressIssue.NONE, validateNodeAddress("http://127.0.0.1:8787"))
        assertEquals(AddressIssue.INSECURE_REMOTE, validateNodeAddress("http://192.168.1.20:8787"))
        assertEquals(AddressIssue.MALFORMED, validateNodeAddress("https://user:pass@node.example"))
        assertEquals(AddressIssue.MALFORMED, validateNodeAddress("https://node.example/base"))
        assertEquals(AddressIssue.MALFORMED, validateNodeAddress("https://node.example?token=leak"))
    }

    @Test
    fun api37RequestsPermissionBeforeEveryLanClassEndpoint() {
        listOf(
            "https://covalent.local:8443",
            "https://node:8443",
            "https://10.0.0.4:8443",
            "https://172.20.0.4:8443",
            "https://192.168.1.4:8443",
            "https://100.100.10.2:8443",
            "https://[fd00::4]:8443",
        ).forEach { assertTrue(it, requiresLocalNetworkPermission(it, 37)) }
        assertFalse(requiresLocalNetworkPermission("http://127.0.0.1:8787", 37))
        assertFalse(requiresLocalNetworkPermission("https://node.example:8443", 37))
        assertFalse(requiresLocalNetworkPermission("https://192.168.1.4:8443", 36))
    }

    @Test
    fun providerConnectionsRequireExactNumericSocketAddresses() {
        assertTrue(validProviderSocketAddress("192.168.1.20:8787"))
        assertTrue(validProviderSocketAddress("[fd00::20]:8787"))
        assertFalse(validProviderSocketAddress("node.local:8787"))
        assertFalse(validProviderSocketAddress("192.168.1.20:0"))
        assertFalse(validProviderSocketAddress("192.168.1.20"))
    }

    @Test
    fun sha256PinsAcceptOnlyExactHexOrStandardPinSyntax() {
        val bytes = ByteArray(32) { it.toByte() }
        val hex = bytes.joinToString("") { "%02x".format(it) }
        assertEquals(hex, normalizeSha256Pin(hex.uppercase()))
        assertEquals(hex, normalizeSha256Pin("sha256/${Base64.getEncoder().encodeToString(bytes)}"))
        assertThrows(IllegalArgumentException::class.java) { normalizeSha256Pin("sha256/not-a-pin") }
        assertThrows(IllegalArgumentException::class.java) { normalizeSha256Pin("00") }
    }

    @Test
    fun enrolledPrivateCaAndPinKeepHostnameVerification() {
        val root = HeldCertificate.Builder()
            .certificateAuthority(1)
            .commonName("Covalent test root")
            .build()
        val serverCertificate = HeldCertificate.Builder()
            .commonName("localhost")
            .addSubjectAlternativeName("localhost")
            .signedBy(root)
            .build()
        val serverCertificates = HandshakeCertificates.Builder()
            .heldCertificate(serverCertificate, root.certificate)
            .build()
        val server = MockWebServer().apply {
            useHttps(serverCertificates.sslSocketFactory(), false)
            repeat(4) {
                enqueue(MockResponse().setBody(
                    """{"deviceName":"TLS node","protocolVersion":1,"lanDiscovery":false,"platformTier":"tier1","state":"ready"}""",
                ))
            }
            start()
        }
        try {
            val baseUrl = server.url("/").toString().removeSuffix("/")
            assertTrue(runCatching { CovalentNodeClient().status(baseUrl) }.isFailure)

            val rootDer = Base64.getEncoder().encodeToString(root.certificate.encoded)
            val caStatus = CovalentNodeClient {
                EnrolledTrust(caCertificateDerBase64 = rootDer)
            }.status(baseUrl)
            assertEquals("TLS node", caStatus.deviceName)

            val pin = MessageDigest.getInstance("SHA-256")
                .digest(serverCertificate.certificate.encoded)
                .joinToString("") { "%02x".format(it) }
            val pinnedClient = CovalentNodeClient { EnrolledTrust(sha256Pin = pin) }
            assertEquals("TLS node", pinnedClient.status(baseUrl).deviceName)

            val rootPin = MessageDigest.getInstance("SHA-256")
                .digest(root.certificate.encoded)
                .joinToString("") { "%02x".format(it) }
            assertEquals(
                "TLS node",
                CovalentNodeClient { EnrolledTrust(sha256Pin = rootPin) }.status(baseUrl).deviceName,
            )

            val wrongHost = "https://127.0.0.1:${server.port}"
            assertTrue(runCatching { pinnedClient.status(wrongHost) }.isFailure)
        } finally {
            server.shutdown()
        }
    }

    @Test
    fun safeWorkflowFieldsRestoreThroughSavedStateHandle() {
        val handle = SavedStateHandle()
        CovalentViewModel(handle).apply {
            screen = Screen.RESTORE
            selectedSourceText = "content://documents/source"
            selectedTargetText = "content://documents/target"
            selectedRestoreBackupId = "backup-safe-id"
            pairingRole = life.michaelwong.covalent.ui.PairingRole.RESPONDER
            pendingPermissionSetup = true
            pendingPermissionDiscovery = true
        }
        CovalentViewModel(handle).apply {
            assertEquals(Screen.RESTORE, screen)
            assertEquals("content://documents/source", selectedSourceText)
            assertEquals("content://documents/target", selectedTargetText)
            assertEquals("backup-safe-id", selectedRestoreBackupId)
            assertEquals(life.michaelwong.covalent.ui.PairingRole.RESPONDER, pairingRole)
            assertTrue(pendingPermissionSetup)
            assertTrue(pendingPermissionDiscovery)
        }
    }

    @Test
    fun settingsImportPreviewFlagsDestructiveBackupRemoval() {
        val current = JSONObject()
            .put("deviceName", "Before")
            .put("lanDiscoveryEnabled", true)
            .put("rememberedBackups", JSONArray().put(JSONObject()).put(JSONObject()))
        val candidate = JSONObject()
            .put("deviceName", "After")
            .put("lanDiscoveryEnabled", false)
            .put("rememberedBackups", JSONArray().put(JSONObject()))
        val preview = settingsPreview(current, candidate)
        assertEquals("Before", preview.oldName)
        assertEquals("After", preview.newName)
        assertTrue(preview.removesBackups)
    }

    @Test
    fun expiredPairingSessionCannotBeConfirmed() {
        val session = JSONObject().put(
            "invitation",
            JSONObject().put("expiresAtUnixMs", 99L),
        )
        assertTrue(isExpired(session, 100L))
        assertFalse(isExpired(session, 98L))
    }

    @Test
    fun durableRestorePreviewUsesBoundedPagesAndPlanIdExecution() {
        val server = MockWebServer().apply {
            enqueue(MockResponse().setBody(restoreReferenceJson(totalEntries = 3).toString()))
            enqueue(MockResponse().setBody(
                restorePageJson(offset = 0, nextCursor = "2", "one.txt", "two.txt").toString(),
            ))
            enqueue(MockResponse().setBody(
                restorePageJson(offset = 2, nextCursor = null, "three.txt").toString(),
            ))
            start()
        }
        try {
            val client = CovalentNodeClient()
            val baseUrl = server.url("/").toString().removeSuffix("/")
            val first = client.previewArchiveRestore(
                baseUrl,
                "token",
                JSONObject()
                    .put("backupId", "backup-1")
                    .put("snapshotId", "snapshot-1")
                    .put("conflictPolicy", "fail")
                    .put("jobId", "restore-job"),
                pageSize = 2,
            )
            assertEquals(listOf("one.txt", "two.txt"), first.entries.map { it.destinationPath })
            assertEquals("2", first.nextCursor)
            assertEquals(
                "/api/v1/restores/archive/preview",
                server.takeRequest().path,
            )
            assertEquals(
                "/api/v1/restores/plans/0123456789abcdef0123456789abcdef?limit=2",
                server.takeRequest().path,
            )

            val second = client.restorePlanPage(baseUrl, "token", first.reference, first.nextCursor, 2)
            assertEquals(listOf("three.txt"), second.entries.map { it.destinationPath })
            assertEquals(2L, second.entryOffset)
            assertEquals(null, second.nextCursor)
            assertEquals(
                "/api/v1/restores/plans/0123456789abcdef0123456789abcdef?limit=2&cursor=2",
                server.takeRequest().path,
            )

            val execution = restoreTransferPayload(first.reference)
            assertEquals(
                "0123456789abcdef0123456789abcdef",
                execution.getJSONObject("restoreRequest").getString("planId"),
            )
            assertFalse(execution.getJSONObject("restoreRequest").has("plan"))

            val restored = restorePlanPageFromPersistence(second.toPersistenceJson())
            assertEquals(second, restored)
        } finally {
            server.shutdown()
        }
    }

    @Test
    fun legacyInlineRestorePlanRemainsCompatibleWithoutExtraNetworkPage() {
        val legacy = restoreReferenceJson(totalEntries = 2).apply {
            remove("planId")
            remove("totalEntries")
            put("entries", JSONArray()
                .put(restoreEntry("one.txt"))
                .put(restoreEntry("two.txt")))
        }
        val server = MockWebServer().apply {
            enqueue(MockResponse().setBody(legacy.toString()))
            start()
        }
        try {
            val preview = CovalentNodeClient().previewArchiveRestore(
                server.url("/").toString().removeSuffix("/"),
                "token",
                JSONObject(),
                pageSize = 1,
            )
            assertEquals(1, server.requestCount)
            assertEquals("1", preview.nextCursor)
            val execution = restoreTransferPayload(preview.reference).getJSONObject("restoreRequest")
            assertTrue(execution.has("plan"))
            assertFalse(execution.has("planId"))
        } finally {
            server.shutdown()
        }
    }

    @Test
    fun retainedArchiveJobsRequireExplicitAcknowledgeAndDiscardEndpoints() {
        assertThrows(IllegalStateException::class.java) { requireJobAcknowledgement(null) }
        assertThrows(IllegalStateException::class.java) { requireJobAcknowledgement("false") }
        requireJobAcknowledgement("true")

        val server = MockWebServer().apply {
            enqueue(MockResponse().setResponseCode(204))
            enqueue(MockResponse().setResponseCode(204))
            start()
        }
        try {
            val client = CovalentNodeClient()
            val baseUrl = server.url("/").toString().removeSuffix("/")
            client.acknowledgeJob(baseUrl, "token", "backup-job")
            client.discardJob(baseUrl, "token", "restore-job")

            server.takeRequest().let { request ->
                assertEquals("/api/v1/jobs/acknowledge", request.path)
                assertEquals("backup-job", JSONObject(request.body.readUtf8()).getString("jobId"))
            }
            server.takeRequest().let { request ->
                assertEquals("/api/v1/jobs/discard", request.path)
                assertEquals("restore-job", JSONObject(request.body.readUtf8()).getString("jobId"))
            }
        } finally {
            server.shutdown()
        }
    }

    private fun restoreReferenceJson(totalEntries: Int): JSONObject = JSONObject()
        .put("planId", "0123456789abcdef0123456789abcdef")
        .put("planDigest", "abcdef0123456789abcdef0123456789")
        .put("backupId", "backup-1")
        .put("snapshotId", "snapshot-1")
        .put("authorizedRoot", "/private/restore-job/target")
        .put("manifestDigest", "manifest-digest")
        .put("conflictPolicy", "fail")
        .put("jobId", "restore-job")
        .put("signerDeviceId", "node-1")
        .put("signature", "signature")
        .put("totalEntries", totalEntries)

    private fun restorePageJson(offset: Int, nextCursor: String?, vararg paths: String): JSONObject =
        restoreReferenceJson(totalEntries = 3)
            .put("entryOffset", offset)
            .put("nextCursor", nextCursor)
            .put("entries", JSONArray().apply { paths.forEach { put(restoreEntry(it)) } })

    private fun restoreEntry(path: String): JSONObject = JSONObject()
        .put("destinationPath", path)
        .put("kind", "file")
        .put("action", "create_file")
}
