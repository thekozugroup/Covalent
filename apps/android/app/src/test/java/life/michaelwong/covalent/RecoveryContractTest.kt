package life.michaelwong.covalent

import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import java.util.Base64
import life.michaelwong.covalent.data.CovalentNodeClient
import life.michaelwong.covalent.data.RecoveryExportMaterial
import life.michaelwong.covalent.data.RecoveryFileException
import life.michaelwong.covalent.data.RecoveryBootstrapMaterial
import life.michaelwong.covalent.data.decodeRecoveryCode
import life.michaelwong.covalent.data.readBounded
import life.michaelwong.covalent.data.writeAndVerify
import life.michaelwong.covalent.model.RecoveryPhase
import life.michaelwong.covalent.node.EmbeddedRecoveryRequest
import life.michaelwong.covalent.node.RecoveryBootstrapHandoff
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class RecoveryContractTest {
    @Test
    fun recoveryExportUsesConfirmedEndpointAndClearsOwnedMaterial() {
        val rawKit = "encrypted-signed-kit".encodeToByteArray()
        val rawKey = ByteArray(32) { it.toByte() }
        val kitText = Base64.getUrlEncoder().withoutPadding().encodeToString(rawKit)
        val codeText = Base64.getUrlEncoder().withoutPadding().encodeToString(rawKey)
        MockWebServer().use { server ->
            server.enqueue(MockResponse().setBody(
                """{"protocolVersion":1,"recoveryKit":"$kitText","recoveryKey":"$codeText"}""",
            ))
            val material = CovalentNodeClient().exportRecoveryKit(
                server.url("/").toString().removeSuffix("/"),
                "test-token-with-more-than-thirty-two-characters",
            )
            assertEquals("RecoveryExportMaterial([REDACTED])", material.toString())
            val request = server.takeRequest()
            assertEquals("/api/v1/recovery/kit", request.path)
            assertEquals("POST", request.method)
            assertEquals("{\"confirmed\":true}", request.body.readUtf8())

            val ownedKit = material.secretField("kit")
            val ownedCode = material.secretField("code")
            assertTrue(ownedKit.contentEquals(rawKit))
            assertTrue(ownedCode.contentEquals(codeText.encodeToByteArray()))
            material.close()
            assertTrue(ownedKit.all { it == 0.toByte() })
            assertTrue(ownedCode.all { it == 0.toByte() })
        }
        rawKey.fill(0)
    }

    @Test
    fun recoveryExportRejectsUnknownFieldsAndMalformedCode() {
        val kitText = Base64.getUrlEncoder().withoutPadding().encodeToString(byteArrayOf(1, 2, 3))
        MockWebServer().use { server ->
            server.enqueue(MockResponse().setBody(
                """{"protocolVersion":1,"recoveryKit":"$kitText","recoveryKey":"password"}""",
            ))
            assertThrows(IllegalArgumentException::class.java) {
                CovalentNodeClient().exportRecoveryKit(
                    server.url("/").toString().removeSuffix("/"),
                    "test-token-with-more-than-thirty-two-characters",
                )
            }
            val code = Base64.getUrlEncoder().withoutPadding().encodeToString(ByteArray(32))
            server.enqueue(MockResponse().setBody(
                """{"protocolVersion":1,"recoveryKit":"$kitText","recoveryKey":"$code","extra":true}""",
            ))
            assertThrows(IllegalArgumentException::class.java) {
                CovalentNodeClient().exportRecoveryKit(
                    server.url("/").toString().removeSuffix("/"),
                    "test-token-with-more-than-thirty-two-characters",
                )
            }
        }
    }

    @Test
    fun recoveryCodeFileAcceptsCanonicalCodeAndRejectsPasswords() {
        val expected = ByteArray(32) { (it + 7).toByte() }
        val text = Base64.getUrlEncoder().withoutPadding().encodeToString(expected)
        val decoded = decodeRecoveryCode("\n$text\r\n".encodeToByteArray())
        assertTrue(decoded.contentEquals(expected))
        decoded.fill(0)
        expected.fill(0)
        assertThrows(IllegalArgumentException::class.java) {
            decodeRecoveryCode("correct horse battery staple".encodeToByteArray())
        }
    }

    @Test
    fun recoveryStatusIsStrictAndRetryUsesConfirmation() {
        val providerId = "00000000-0000-4000-8000-000000000001"
        val backupId = "00000000-0000-4000-8000-000000000002"
        val body = """{
            "protocolVersion":1,
            "phase":"partial",
            "recoveredBackups":[{"backupId":"$backupId","snapshotId":"snapshot_1","sourceProviderIds":["$providerId"]}],
            "queriedProviderIds":["$providerId"],
            "configuredProviderIds":["$providerId"],
            "failures":[{"providerId":"$providerId","snapshotId":null,"reason":"offline"}],
            "newerSnapshotMayExist":true
        }""".trimIndent()
        MockWebServer().use { server ->
            server.enqueue(MockResponse().setBody(body))
            server.enqueue(MockResponse().setBody(
                body
                    .replace("\"phase\":\"partial\"", "\"phase\":\"imported\"")
                    .replace(
                        "\"failures\":[{\"providerId\":\"$providerId\",\"snapshotId\":null,\"reason\":\"offline\"}]",
                        "\"failures\":[]",
                    )
                    .replace("\"newerSnapshotMayExist\":true", "\"newerSnapshotMayExist\":false"),
            ))
            val client = CovalentNodeClient()
            val baseUrl = server.url("/").toString().removeSuffix("/")
            val status = client.recoveryStatus(baseUrl, "test-token-with-more-than-thirty-two-characters")
            assertEquals(RecoveryPhase.PARTIAL, status.phase)
            assertEquals(1, status.recoveredBackups.size)
            assertTrue(status.newerSnapshotMayExist)
            assertTrue(status.phase.canRetry)

            val retried = client.retryRecovery(baseUrl, "test-token-with-more-than-thirty-two-characters")
            assertEquals(RecoveryPhase.IMPORTED, retried.phase)
            assertFalse(retried.phase.canRetry)
            assertEquals("/api/v1/recovery/status", server.takeRequest().path)
            val retryRequest = server.takeRequest()
            assertEquals("/api/v1/recovery/retry", retryRequest.path)
            assertEquals("{\"confirmed\":true}", retryRequest.body.readUtf8())
        }
    }

    @Test
    fun recoveryStatusRejectsForeignProvidersAndContradictorySuccess() {
        val configured = "00000000-0000-4000-8000-000000000001"
        val foreign = "00000000-0000-4000-8000-000000000002"
        val backup = "00000000-0000-4000-8000-000000000003"
        fun statusBody(sourceProvider: String, newerSnapshotMayExist: Boolean): String = """{
            "protocolVersion":1,
            "phase":"imported",
            "recoveredBackups":[{"backupId":"$backup","snapshotId":"snapshot_1","sourceProviderIds":["$sourceProvider"]}],
            "queriedProviderIds":["$configured"],
            "configuredProviderIds":["$configured"],
            "failures":[],
            "newerSnapshotMayExist":$newerSnapshotMayExist
        }""".trimIndent()

        MockWebServer().use { server ->
            server.enqueue(MockResponse().setBody(statusBody(foreign, false)))
            server.enqueue(MockResponse().setBody(statusBody(configured, true)))
            val client = CovalentNodeClient()
            val baseUrl = server.url("/").toString().removeSuffix("/")
            assertThrows(IllegalArgumentException::class.java) {
                client.recoveryStatus(baseUrl, "test-token-with-more-than-thirty-two-characters")
            }
            assertThrows(IllegalArgumentException::class.java) {
                client.recoveryStatus(baseUrl, "test-token-with-more-than-thirty-two-characters")
            }
        }
    }

    @Test
    fun boundedReadsAndVerifiedWritesFailClosedOnTruncation() {
        assertThrows(RecoveryFileException::class.java) {
            ByteArrayInputStream(ByteArray(9)).readBounded(8, "recovery input")
        }
        val bytes = "complete recovery file".encodeToByteArray()
        writeAndVerify(
            bytes,
            "recovery kit",
            openOutput = { ByteArrayOutputStream() },
            openInput = { ByteArrayInputStream(bytes) },
        )
        assertThrows(RecoveryFileException::class.java) {
            writeAndVerify(
                bytes,
                "recovery kit",
                openOutput = { ByteArrayOutputStream() },
                openInput = { ByteArrayInputStream(bytes.copyOf(bytes.size - 1)) },
            )
        }
    }

    @Test
    fun partialExportRetryKeepsOnlyTheStillNeededEncryptedKit() {
        val rawKey = ByteArray(32) { (it + 11).toByte() }
        val code = Base64.getUrlEncoder().withoutPadding().encode(rawKey)
        val material = RecoveryExportMaterial("kit".encodeToByteArray(), code)
        val kitBytes = material.secretField("kit")
        val codeBytes = material.secretField("code")
        assertThrows(RecoveryFileException::class.java) {
            material.saveWith(
                codeWriter = { },
                kitWriter = { throw RecoveryFileException("partial write") },
            )
        }
        assertTrue(material.codeSaved)
        assertFalse(material.kitSaved)
        assertTrue(codeBytes.all { it == 0.toByte() })
        assertTrue(kitBytes.any { it != 0.toByte() })

        material.saveWith(codeWriter = { error("code must not be written twice") }, kitWriter = { })
        assertTrue(material.complete)
        assertTrue(kitBytes.all { it == 0.toByte() })
        material.close()
        rawKey.fill(0)
    }

    @Test
    fun serviceHandoffIsSingleUseAndPendingCancellationClearsSecrets() {
        RecoveryBootstrapHandoff.cancel()
        val pending = RecoveryBootstrapMaterial(byteArrayOf(1, 2), ByteArray(32) { 9.toByte() })
        assertTrue(
            RecoveryBootstrapHandoff.offer(EmbeddedRecoveryRequest(pending, 1_000, 100, false)),
        )
        assertTrue(RecoveryBootstrapHandoff.isActive())
        RecoveryBootstrapHandoff.cancelPending()
        assertFalse(RecoveryBootstrapHandoff.isActive())
        assertTrue(pending.secretField("kit").all { it == 0.toByte() })
        assertTrue(pending.secretField("key").all { it == 0.toByte() })

        val inFlight = RecoveryBootstrapMaterial(byteArrayOf(3), ByteArray(32) { 7.toByte() })
        assertTrue(
            RecoveryBootstrapHandoff.offer(EmbeddedRecoveryRequest(inFlight, 1_000, 100, false)),
        )
        val consumed = checkNotNull(RecoveryBootstrapHandoff.take())
        RecoveryBootstrapHandoff.cancelPending()
        assertTrue(RecoveryBootstrapHandoff.isActive())
        assertTrue(inFlight.secretField("key").any { it != 0.toByte() })
        consumed.close()
        RecoveryBootstrapHandoff.finish()
        assertFalse(RecoveryBootstrapHandoff.isActive())
        assertTrue(inFlight.secretField("key").all { it == 0.toByte() })
    }

    private fun RecoveryExportMaterial.secretField(name: String): ByteArray =
        javaClass.getDeclaredField(name).let { field ->
            field.isAccessible = true
            field.get(this) as ByteArray
        }

    private fun RecoveryBootstrapMaterial.secretField(name: String): ByteArray =
        javaClass.getDeclaredField(name).let { field ->
            field.isAccessible = true
            field.get(this) as ByteArray
        }
}
