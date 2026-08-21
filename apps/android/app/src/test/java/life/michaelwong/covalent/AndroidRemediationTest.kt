package life.michaelwong.covalent

import androidx.lifecycle.SavedStateHandle
import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import java.util.Base64
import java.security.MessageDigest
import java.util.zip.ZipEntry
import java.util.zip.ZipInputStream
import java.util.zip.ZipOutputStream
import life.michaelwong.covalent.data.CovalentNodeClient
import life.michaelwong.covalent.data.EnrolledTrust
import life.michaelwong.covalent.data.NodeApiException
import life.michaelwong.covalent.data.archiveUploadRetryOffset
import life.michaelwong.covalent.data.normalizeSha256Pin
import life.michaelwong.covalent.data.restorePlanPageFromPersistence
import life.michaelwong.covalent.data.restoreTransferPayload
import life.michaelwong.covalent.data.sameSignedRestorePlan
import life.michaelwong.covalent.data.targetInventoryPageDigest
import life.michaelwong.covalent.data.toPersistenceJson
import life.michaelwong.covalent.data.writeZeroByteZipContent
import life.michaelwong.covalent.model.RestorePlanReference
import life.michaelwong.covalent.model.RestoreConflictPolicy
import life.michaelwong.covalent.model.TargetInventoryDraft
import life.michaelwong.covalent.model.TargetInventoryEntry
import life.michaelwong.covalent.model.TargetInventoryBinding
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
    fun archiveUploadResumesOnlyAtBoundedRetryableServerOffsets() {
        val resumable = NodeApiException(409, 1, "upload_incomplete", true, "resume", 42)
        assertEquals(42L, archiveUploadRetryOffset(resumable, 100))
        assertEquals(null, archiveUploadRetryOffset(NodeApiException(409, 1, "upload_incomplete", true, "too far", 101), 100))
        assertEquals(null, archiveUploadRetryOffset(NodeApiException(409, 1, "upload_identity_mismatch", false, "terminal", 42), 100))
    }

    @Test
    fun networkPairingClientUsesTypedStrictContract() {
        val server = MockWebServer().apply {
            enqueue(MockResponse().setBody(networkPairingJson("awaiting_local_confirmation").toString()))
            enqueue(MockResponse().setBody(JSONArray().put(networkPairingJson("awaiting_peer_confirmation")).toString()))
            enqueue(MockResponse().setBody(networkPairingJson("complete", true).toString()))
            enqueue(MockResponse().setResponseCode(204))
            start()
        }
        try {
            val client = CovalentNodeClient()
            val baseUrl = server.url("/").toString().removeSuffix("/")
            assertEquals("pair-1", client.startNetworkPairing(baseUrl, "token", "192.168.1.20:8787").pairingId)
            assertEquals(1, client.pendingNetworkPairings(baseUrl, "token").size)
            assertEquals("COMPLETE", client.confirmNetworkPairing(baseUrl, "token", "pair-1", "1234-5678-9012-3456").state.name)
            client.cancelNetworkPairing(baseUrl, "token", "pair-1")
            val start = server.takeRequest()
            assertEquals("/api/v1/pair/network/start", start.path)
            assertEquals("192.168.1.20:8787", JSONObject(start.body.readUtf8()).getString("candidateAddress"))
            assertEquals("/api/v1/pair/network/pending", server.takeRequest().path)
            assertEquals("/api/v1/pair/network/pair-1/confirm", server.takeRequest().path)
            assertEquals("/api/v1/pair/network/pair-1", server.takeRequest().path)
        } finally { server.shutdown() }
    }

    @Test
    fun networkPairingRejectsMalformedConfirmationAndResponses() {
        assertThrows(IllegalArgumentException::class.java) {
            CovalentNodeClient().confirmNetworkPairing("http://127.0.0.1:1", "token", "bad/id", "1234-5678-9012-3456")
        }
        val server = MockWebServer().apply {
            enqueue(MockResponse().setBody(networkPairingJson("complete", false).toString()))
            start()
        }
        try {
            assertThrows(IllegalStateException::class.java) {
                CovalentNodeClient().startNetworkPairing(server.url("/").toString().removeSuffix("/"), "token", "192.168.1.20:8787")
            }
        } finally { server.shutdown() }
    }
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
            selectedRestorePolicy = RestoreConflictPolicy.RENAME
            pairingRole = life.michaelwong.covalent.ui.PairingRole.RESPONDER
            pendingPermissionSetup = true
            pendingPermissionDiscovery = true
        }
        CovalentViewModel(handle).apply {
            assertEquals(Screen.RESTORE, screen)
            assertEquals("content://documents/source", selectedSourceText)
            assertEquals("content://documents/target", selectedTargetText)
            assertEquals("backup-safe-id", selectedRestoreBackupId)
            assertEquals(RestoreConflictPolicy.RENAME, selectedRestorePolicy)
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
                restorePageJson(0, 3, "2", "one.txt", "two.txt").toString(),
            ))
            enqueue(MockResponse().setBody(
                restorePageJson(2, 3, null, "three.txt").toString(),
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
    fun targetInventoryUsesExactDigestAndPagedImmutableWireContract() {
        val entries = listOf(
            TargetInventoryEntry("a.txt", "file", 3, 1_000, "id-a"),
            TargetInventoryEntry("folder", "directory", 0, null, "id-d"),
        )
        assertEquals(
            "6b27d4b7e74c3a6a9680a300c74191e9580c17370526a2fc5b9c55a0eea4ea93",
            targetInventoryPageDigest(entries),
        )
        val inventoryId = "a".repeat(64)
        val inventoryDigest = "b".repeat(64)
        val server = MockWebServer().apply {
            enqueue(MockResponse().setBody(JSONObject()
                .put("inventoryId", inventoryId)
                .put("jobId", "restore-job")
                .put("nextOffset", 0).toString()))
            enqueue(MockResponse().setBody(JSONObject()
                .put("inventoryId", inventoryId)
                .put("jobId", "restore-job")
                .put("nextOffset", 2).toString()))
            enqueue(MockResponse().setBody(JSONObject()
                .put("inventoryId", inventoryId)
                .put("jobId", "restore-job")
                .put("schemaVersion", 1)
                .put("rootIdentity", "saf-tree-sha256=${"c".repeat(64)}")
                .put("entryCount", 2)
                .put("totalBytes", 3)
                .put("inventoryDigest", inventoryDigest).toString()))
            start()
        }
        try {
            val draft = TargetInventoryDraft(
                rootIdentity = "saf-tree-sha256=${"c".repeat(64)}",
                totalBytes = 3,
                entries = entries,
            )
            val reference = CovalentNodeClient().uploadTargetInventory(
                server.url("/").toString().removeSuffix("/"),
                "token",
                "restore-job",
                draft,
                pageSize = 2,
            )
            assertEquals(inventoryDigest, reference.inventoryDigest)
            server.takeRequest().let { request ->
                assertEquals("/api/v1/restores/archive/inventories", request.path)
                assertEquals(2, JSONObject(request.body.readUtf8()).getInt("entryCount"))
            }
            server.takeRequest().let { request ->
                assertEquals("/api/v1/restores/archive/inventories/$inventoryId/pages", request.path)
                val body = JSONObject(request.body.readUtf8())
                assertEquals(0, body.getInt("offset"))
                assertEquals(targetInventoryPageDigest(entries), body.getString("pageDigest"))
                assertFalse(body.getJSONArray("entries").getJSONObject(1).has("modifiedAtUnixMs"))
            }
            server.takeRequest().let { request ->
                assertEquals("/api/v1/restores/archive/inventories/$inventoryId/finalize", request.path)
                assertEquals("", JSONObject(request.body.readUtf8()).getString("inventoryDigest"))
            }
        } finally { server.shutdown() }
    }

    @Test
    fun workerRebindRejectsAnyChangedSignedInventoryOrActionSet() {
        val binding = TargetInventoryBinding(
            schemaVersion = 1,
            rootIdentity = "saf-tree-sha256=${"c".repeat(64)}",
            entryCount = 2,
            totalBytes = 3,
            inventoryDigest = "d".repeat(64),
            actionsDigest = "e".repeat(64),
        )
        val reference = RestorePlanReference(
            planId = "0123456789abcdef0123456789abcdef",
            planDigest = "f".repeat(64),
            backupId = "backup-1",
            snapshotId = "snapshot-1",
            authorizedRoot = "/private/restore-job/target",
            manifestDigest = "a".repeat(64),
            conflictPolicy = "rename",
            jobId = "restore-job",
            signerDeviceId = "node-1",
            signature = "signature",
            totalEntries = 2,
            targetInventory = binding,
        )
        assertTrue(sameSignedRestorePlan(reference, reference.copy()))
        assertFalse(sameSignedRestorePlan(
            reference,
            reference.copy(targetInventory = binding.copy(actionsDigest = "0".repeat(64))),
        ))
        assertFalse(sameSignedRestorePlan(
            reference,
            reference.copy(targetInventory = binding.copy(inventoryDigest = "1".repeat(64))),
        ))
    }

    @Test
    fun targetInventoryResumesOnlyAtBoundedAuthoritativeOffset() {
        val inventoryId = "2".repeat(64)
        val rootIdentity = "saf-tree-sha256=${"3".repeat(64)}"
        val entries = listOf(
            TargetInventoryEntry("a.txt", "file", 1, null, "id-a"),
            TargetInventoryEntry("b.txt", "file", 2, null, "id-b"),
        )
        val server = MockWebServer().apply {
            enqueue(MockResponse().setBody(JSONObject()
                .put("inventoryId", inventoryId)
                .put("jobId", "restore-job")
                .put("nextOffset", 0).toString()))
            enqueue(MockResponse()
                .setResponseCode(409)
                .setHeader("X-Covalent-Inventory-Offset", "1")
                .setBody(JSONObject()
                    .put("protocolVersion", 1)
                    .put("code", "target_inventory_offset_mismatch")
                    .put("retryable", false)
                    .put("message", "resume").toString()))
            enqueue(MockResponse().setBody(JSONObject()
                .put("inventoryId", inventoryId)
                .put("jobId", "restore-job")
                .put("nextOffset", 2).toString()))
            enqueue(MockResponse().setBody(JSONObject()
                .put("inventoryId", inventoryId)
                .put("jobId", "restore-job")
                .put("schemaVersion", 1)
                .put("rootIdentity", rootIdentity)
                .put("entryCount", 2)
                .put("totalBytes", 3)
                .put("inventoryDigest", "4".repeat(64)).toString()))
            start()
        }
        try {
            CovalentNodeClient().uploadTargetInventory(
                server.url("/").toString().removeSuffix("/"),
                "token",
                "restore-job",
                TargetInventoryDraft(rootIdentity, 3, entries),
                pageSize = 2,
            )
            server.takeRequest()
            val initialPage = JSONObject(server.takeRequest().body.readUtf8())
            val resumedPage = JSONObject(server.takeRequest().body.readUtf8())
            assertEquals(0, initialPage.getInt("offset"))
            assertEquals(1, resumedPage.getInt("offset"))
            assertEquals(1, resumedPage.getJSONArray("entries").length())
            assertEquals("b.txt", resumedPage.getJSONArray("entries").getJSONObject(0).getString("path"))
        } finally { server.shutdown() }
    }

    @Test
    fun renamePreviewPersistsSignedInventoryBindingAndReplaceFailsClosed() {
        fun boundReference(policy: String): JSONObject = restoreReferenceJson(totalEntries = 1)
            .put("conflictPolicy", policy)
            .put("targetInventory", JSONObject()
                .put("schemaVersion", 1)
                .put("rootIdentity", "saf-tree-sha256=${"5".repeat(64)}")
                .put("entryCount", 1)
                .put("totalBytes", 4)
                .put("inventoryDigest", "6".repeat(64))
                .put("actionsDigest", "7".repeat(64)))
        val renameReference = boundReference("rename")
        val replaceReference = boundReference("replace")
        val server = MockWebServer().apply {
            enqueue(MockResponse().setBody(renameReference.toString()))
            enqueue(MockResponse().setBody(JSONObject(renameReference.toString())
                .put("entryOffset", 0)
                .put("nextCursor", JSONObject.NULL)
                .put("entries", JSONArray().put(restoreEntry("note (restored 1).txt", "rename_file")))
                .toString()))
            enqueue(MockResponse().setBody(replaceReference.toString()))
            start()
        }
        try {
            val client = CovalentNodeClient()
            val baseUrl = server.url("/").toString().removeSuffix("/")
            val rename = client.previewArchiveRestore(baseUrl, "token", JSONObject())
            assertEquals("rename_file", rename.entries.single().action)
            assertEquals("6".repeat(64), rename.reference.targetInventory?.inventoryDigest)
            assertEquals(rename, restorePlanPageFromPersistence(rename.toPersistenceJson()))
            assertThrows(IllegalStateException::class.java) {
                client.previewArchiveRestore(baseUrl, "token", JSONObject())
            }
        } finally { server.shutdown() }
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

    @Test
    fun restorePageRejectsOverflowingOffsetBeforeSignedPlanComparison() {
        val server = MockWebServer().apply {
            enqueue(MockResponse().setBody(
                restorePageJson(
                    offset = Long.MAX_VALUE,
                    totalEntries = 100_000L,
                    nextCursor = null,
                    "one.txt",
                ).toString(),
            ))
            start()
        }
        try {
            val reference = RestorePlanReference(
                planId = "0123456789abcdef0123456789abcdef",
                planDigest = "abcdef0123456789abcdef0123456789",
                backupId = "backup-1",
                snapshotId = "snapshot-1",
                authorizedRoot = "/private/restore-job/target",
                manifestDigest = "manifest-digest",
                conflictPolicy = "fail",
                jobId = "restore-job",
                signerDeviceId = "node-1",
                signature = "signature",
                totalEntries = 100_000,
            )
            assertThrows(IllegalStateException::class.java) {
                CovalentNodeClient().restorePlanPage(
                    server.url("/").toString().removeSuffix("/"),
                    "token",
                    reference,
                    cursor = null,
                    limit = 1,
                )
            }
        } finally {
            server.shutdown()
        }
    }

    @Test
    fun zeroByteZipWritesPreserveEmptyDirectoryAndFileSemantics() {
        val bytes = ByteArrayOutputStream().use { output ->
            ZipOutputStream(output).use { archive ->
                archive.putNextEntry(ZipEntry("empty/"))
                writeZeroByteZipContent(archive)
                archive.closeEntry()
                archive.putNextEntry(ZipEntry("empty/file.bin"))
                writeZeroByteZipContent(archive)
                archive.closeEntry()
            }
            output.toByteArray()
        }
        ZipInputStream(ByteArrayInputStream(bytes)).use { archive ->
            val directory = requireNotNull(archive.nextEntry)
            assertTrue(directory.isDirectory)
            assertEquals(-1, archive.read())
            archive.closeEntry()
            val file = requireNotNull(archive.nextEntry)
            assertFalse(file.isDirectory)
            assertEquals(-1, archive.read())
        }
    }

    private fun restoreReferenceJson(totalEntries: Long): JSONObject = JSONObject()
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

    private fun restorePageJson(
        offset: Long,
        totalEntries: Long = 3,
        nextCursor: String?,
        vararg paths: String,
    ): JSONObject =
        restoreReferenceJson(totalEntries = totalEntries)
            .put("entryOffset", offset)
            .put("nextCursor", nextCursor)
            .put("entries", JSONArray().apply { paths.forEach { put(restoreEntry(it)) } })

    private fun restoreEntry(path: String, action: String = "create_file"): JSONObject = JSONObject()
        .put("destinationPath", path)
        .put("kind", "file")
        .put("action", action)

    private fun networkPairingJson(state: String, transport: Boolean = false): JSONObject = JSONObject()
        .put("pairingId", "pair-1")
        .put("direction", "outgoing")
        .put("peerName", "NAS")
        .put("authenticationString", "1234-5678-9012-3456")
        .put("expiresAtUnixMs", 123456789L)
        .put("state", state)
        .put("peerTransport", if (transport) JSONObject()
            .put("peerId", "peer-1")
            .put("displayName", "NAS")
            .put("address", "192.168.1.20:8787")
            .put("certificateDer", "AQID")
            .put("certificateFingerprint", "a".repeat(64)) else JSONObject.NULL)
}
