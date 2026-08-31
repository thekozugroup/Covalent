package life.michaelwong.covalent

import java.io.ByteArrayInputStream
import java.io.File
import java.io.IOException
import java.net.ConnectException
import java.security.cert.CertificateException
import javax.net.ssl.SSLHandshakeException
import life.michaelwong.covalent.data.NodeApiException
import life.michaelwong.covalent.data.NodeProtocolException
import life.michaelwong.covalent.ui.AddressIssue
import life.michaelwong.covalent.ui.DestructiveAction
import life.michaelwong.covalent.ui.NodeFailure
import life.michaelwong.covalent.ui.SetupLinkOutcome
import life.michaelwong.covalent.ui.classifyNodeFailure
import life.michaelwong.covalent.ui.destructiveConfirmation
import life.michaelwong.covalent.ui.nodeFailureMessageRes
import life.michaelwong.covalent.ui.parseClaimTokenFile
import life.michaelwong.covalent.ui.readClaimTokenFile
import life.michaelwong.covalent.ui.setupLinkEndpoint
import life.michaelwong.covalent.ui.setupLinkOutcome
import life.michaelwong.covalent.ui.validateNodeAddress
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Copy and safety contracts for the Android surface.
 *
 * These assertions exist because two defects shipped: user-facing copy promised a QR
 * scanner the app does not contain, and every destructive Android action ran without a
 * confirmation while iOS confirmed the same actions. Both are cheap to reintroduce and
 * expensive to notice by hand, so they are pinned here.
 */
class AndroidCopyAndConfirmationTest {

    // ---- Destructive-action confirmations ------------------------------------------------

    @Test
    fun everyDestructiveActionHasCompleteDistinctConfirmationCopy() {
        val confirmations = DestructiveAction.entries.associateWith(::destructiveConfirmation)
        assertEquals(DestructiveAction.entries.size, confirmations.size)
        confirmations.forEach { (action, copy) ->
            listOf(copy.titleRes, copy.messageRes, copy.confirmRes, copy.cancelRes).forEach { id ->
                assertTrue("$action is missing a string resource", id != 0)
            }
            assertTrue(
                "$action reuses one string for two roles",
                setOf(copy.titleRes, copy.messageRes, copy.confirmRes, copy.cancelRes).size == 4,
            )
        }
        val titles = confirmations.values.map { it.titleRes }
        assertEquals("Two destructive actions share a title", titles.size, titles.toSet().size)
        val messages = confirmations.values.map { it.messageRes }
        assertEquals("Two destructive actions share a message", messages.size, messages.toSet().size)
    }

    @Test
    fun destructiveConfirmationsNameTheirSubjectAndTheirConsequence() {
        val subjectNaming = mapOf(
            DestructiveAction.CANCEL_TRANSFER to "confirm_cancel_transfer_message",
            DestructiveAction.CANCEL_DEVICE_REQUEST to "confirm_cancel_device_request_message",
            DestructiveAction.IMPORT_REMOVING_BACKUPS to "confirm_import_removes_backups_message",
        )
        val standalone = mapOf(
            DestructiveAction.DISABLE_PHONE_STORAGE to "confirm_disable_phone_storage_message",
            DestructiveAction.DISCARD_PAIRING_PROGRESS to "confirm_discard_pairing_message",
        )
        assertEquals(
            "Every destructive action must be classified by this test",
            DestructiveAction.entries.toSet(),
            subjectNaming.keys + standalone.keys,
        )
        subjectNaming.forEach { (action, name) ->
            assertTrue(
                "$action claims to name its subject",
                destructiveConfirmation(action).namesSubject,
            )
            val message = requireString(name)
            assertTrue("$name must interpolate the thing being destroyed", message.contains("%1\$s"))
        }
        standalone.forEach { (action, name) ->
            assertFalse(
                "$action must not claim a subject placeholder",
                destructiveConfirmation(action).namesSubject,
            )
            assertFalse("$name must not carry an unfilled placeholder", requireString(name).contains("%1\$s"))
        }
        (subjectNaming.values + standalone.values).forEach { name ->
            val message = requireString(name)
            assertTrue(
                "$name must state what the person loses",
                CONSEQUENCE_WORDS.any { message.contains(it, ignoreCase = true) },
            )
            assertTrue("$name is too terse to explain a destructive action", message.length >= 60)
        }
    }

    @Test
    fun destructiveConfirmationsOfferAWayOut() {
        listOf(
            "confirm_cancel_transfer_keep",
            "confirm_disable_phone_storage_keep",
            "confirm_discard_pairing_keep",
            "confirm_cancel_device_request_keep",
            "confirm_import_removes_backups_keep",
        ).forEach { name -> assertTrue("$name must be a real label", requireString(name).isNotBlank()) }
    }

    // ---- The QR promise ------------------------------------------------------------------

    @Test
    fun setupCopyUsesTrustedClaimOutputAndDoesNotAdvertiseAnUnproducedLink() {
        val handoff = requireString("setup_handoff_title") + " " + requireString("setup_handoff_detail")
        listOf("QR", "scan", "camera", "barcode", "covalent://").forEach { claim ->
            assertFalse(
                "Setup copy advertises \"$claim\", which no current claim tool produces",
                handoff.contains(claim, ignoreCase = true),
            )
        }
        assertTrue(handoff.contains("covalent claim", ignoreCase = true))
        assertTrue(handoff.contains("local-api-token"))
        assertTrue(handoff.contains("root.crt"))
        assertTrue(handoff.contains("never accepts a setup code", ignoreCase = true))
    }

    @Test
    fun claimTokenFileIsBoundedAndUsesTheServerTokenShape() {
        val token = "t".repeat(32)
        assertEquals(token, parseClaimTokenFile("  $token\n".encodeToByteArray()))
        assertEquals(token, readClaimTokenFile(ByteArrayInputStream(token.encodeToByteArray())))
        listOf(
            ByteArray(0),
            "short".encodeToByteArray(),
            ("t".repeat(31) + " ").encodeToByteArray(),
            ("t".repeat(31) + "\\").encodeToByteArray(),
            "t".repeat(513).encodeToByteArray(),
            ByteArray(1_025) { 't'.code.toByte() },
            byteArrayOf(0xc3.toByte(), 0x28),
        ).forEach { rejected ->
            assertTrue(runCatching { parseClaimTokenFile(rejected) }.isFailure)
        }
        assertTrue(
            runCatching {
                readClaimTokenFile(ByteArrayInputStream(ByteArray(1_025) { 't'.code.toByte() }))
            }.isFailure,
        )
    }

    @Test
    fun setupGuidanceDoesNotPretendToBeAnActionCard() {
        val source = moduleFile("src/main/java/life/michaelwong/covalent/ui/CovalentApp.kt").readText()
        val choice = source.substringAfter("private fun OnboardingChoice").substringBefore("@Composable")
        assertTrue("Setup guidance must use a plain row", choice.contains("Row("))
        assertFalse("Setup guidance must not use an inert card", choice.contains("OutlinedCard"))
        assertFalse("Setup guidance must not advertise a click action", choice.contains("onClick"))
    }

    @Test
    fun signedConflictPoliciesCanPreviewAndRestoreIntoExistingContent() {
        val ui = moduleFile("src/main/java/life/michaelwong/covalent/ui/CovalentApp.kt").readText()
        val targetCheck = ui
            .substringAfter("private fun ensureWritableTarget")
            .substringBefore("private fun queueTransfer")
        assertFalse(
            "The UI must not reject a non-empty target before signed inventory conflict planning",
            targetCheck.contains("listFiles().isEmpty()"),
        )

        val bridge = moduleFile("src/main/java/life/michaelwong/covalent/data/SafTransferBridge.kt").readText()
        assertTrue("Current restores must re-inventory the real target before execution", bridge.contains(
            "freshInventory = targetInventory(context, targetTree)",
        ))
        assertTrue("Current restores must require the same signed rebound plan", bridge.contains(
            "sameSignedRestorePlan(reference, rebound.reference)",
        ))
        assertTrue(
            "Only legacy unbound restores may retain the empty-folder compatibility gate",
            bridge.contains("This older restore preview requires an empty folder"),
        )
    }

    @Test
    fun manifestReceivesTheSetupLinkTheCopyDescribes() {
        val manifest = moduleFile("src/main/AndroidManifest.xml").readText()
        assertTrue(
            "MainActivity must declare a covalent:// scheme so setup links open the app",
            manifest.contains("android:scheme=\"covalent\""),
        )
        assertTrue(manifest.contains("android:host=\"connect\""))
        assertTrue(manifest.contains("android.intent.action.VIEW"))
        assertTrue(manifest.contains("android.intent.category.BROWSABLE"))
    }

    @Test
    fun setupLinksOnlyPrefillAddressesTheAppWouldAcceptByHand() {
        assertEquals(
            "https://node.example:8443",
            setupLinkEndpoint("covalent://connect?endpoint=https%3A%2F%2Fnode.example%3A8443"),
        )
        assertEquals(
            AddressIssue.NONE,
            validateNodeAddress(setupLinkEndpoint("covalent://connect?endpoint=https%3A%2F%2Fnode.example%3A8443")),
        )
        // Anything that is not a Covalent setup link is ignored outright.
        assertEquals("", setupLinkEndpoint("https://node.example:8443"))
        assertEquals("", setupLinkEndpoint("covalent://pair?endpoint=https%3A%2F%2Fnode.example"))
        assertEquals("", setupLinkEndpoint(""))
        // A link may never smuggle in an address the setup form itself would refuse.
        assertEquals("", setupLinkEndpoint("covalent://connect?endpoint=http%3A%2F%2F192.168.1.20%3A8787"))
        assertEquals("", setupLinkEndpoint("covalent://connect?endpoint=https%3A%2F%2Fuser%3Apass%40node.example"))
        assertEquals("", setupLinkEndpoint("covalent://connect?token=secret"))
    }

    @Test
    fun anOutsideAppCannotPointAConfiguredInstallAtAnotherServer() {
        val link = "covalent://connect?endpoint=https%3A%2F%2Fattacker.example%3A8443"
        assertEquals(
            SetupLinkOutcome.Apply("https://attacker.example:8443"),
            setupLinkOutcome(link, hasSavedServer = false),
        )
        // Once a server is saved, only Settings may change it.
        assertEquals(SetupLinkOutcome.AlreadyConnected, setupLinkOutcome(link, hasSavedServer = true))
        assertEquals(SetupLinkOutcome.Rejected, setupLinkOutcome("covalent://connect?", hasSavedServer = false))
        assertEquals(SetupLinkOutcome.Rejected, setupLinkOutcome("https://elsewhere.example", hasSavedServer = false))
    }

    // ---- Error copy ---------------------------------------------------------------------

    @Test
    fun transportFailuresBecomePlainLanguageInsteadOfRawExceptions() {
        assertEquals(
            NodeFailure.NEEDS_RECONNECT,
            classifyNodeFailure(apiError(401, "unauthorized")),
        )
        assertEquals(NodeFailure.NOT_PERMITTED, classifyNodeFailure(apiError(403, "forbidden")))
        // The outgoing half of network pairing answers 404 on servers without the route.
        assertEquals(NodeFailure.UNSUPPORTED_BY_SERVER, classifyNodeFailure(apiError(404, "http_404")))
        assertEquals(NodeFailure.UNSUPPORTED_BY_SERVER, classifyNodeFailure(NodeProtocolException(9)))
        assertEquals(NodeFailure.SERVER_PROBLEM, classifyNodeFailure(apiError(503, "unavailable")))
        assertEquals(NodeFailure.UNREACHABLE, classifyNodeFailure(ConnectException("ECONNREFUSED")))
        assertEquals(
            NodeFailure.UNREACHABLE,
            classifyNodeFailure(IllegalStateException("wrapped", IOException("timeout"))),
        )
        // SSLException extends IOException; a trust failure must not read as "unreachable".
        assertEquals(
            NodeFailure.UNTRUSTED_CERTIFICATE,
            classifyNodeFailure(SSLHandshakeException("pin mismatch")),
        )
        assertEquals(
            NodeFailure.UNTRUSTED_CERTIFICATE,
            classifyNodeFailure(IOException("chain", CertificateException("pin mismatch"))),
        )
        // A domain error keeps the backup server's own, more specific message.
        assertEquals(NodeFailure.UNKNOWN, classifyNodeFailure(apiError(400, "target_not_empty")))
    }

    @Test
    fun everyRecognisedFailureHasItsOwnPlainMessage() {
        val recognised = NodeFailure.entries.filter { it != NodeFailure.UNKNOWN }
        val ids = recognised.map(::nodeFailureMessageRes)
        assertEquals("Recognised failures must not share copy", ids.size, ids.toSet().size)
        ids.forEach { assertTrue("A failure has no message resource", it != 0) }
        assertEquals(
            "UNKNOWN must fall back to the generic sentence",
            R.string.error_node_action_failed,
            nodeFailureMessageRes(NodeFailure.UNKNOWN),
        )
        listOf(
            "error_server_unreachable",
            "error_server_certificate",
            "error_server_needs_reconnect",
            "error_server_not_permitted",
            "error_server_unsupported",
            "error_server_problem",
        ).forEach { name ->
            val message = requireString(name)
            assertTrue("$name must tell the person what to do", message.contains(". "))
            assertFalse("$name must not expose an exception", message.contains("Exception"))
        }
    }

    // ---- Jargon -------------------------------------------------------------------------

    @Test
    fun noUserFacingStringLeaksEngineVocabulary() {
        val offenders = userFacingCopy().filter { (_, value) ->
            JARGON.any { it.containsMatchIn(value) }
        }
        assertTrue(
            "User-facing copy still contains engine vocabulary: " +
                offenders.entries.joinToString("; ") { "${it.key} = ${it.value}" },
            offenders.isEmpty(),
        )
    }

    @Test
    fun theRoleGrantExplainsItselfInsteadOfPrintingItsToken() {
        val message = requireString("provider_role_not_granted")
        assertFalse(
            "The raw role token must never reach a person",
            message.contains("storage_provider"),
        )
        assertTrue(
            "The message must point at the permission label the pairing screen shows",
            message.contains(requireString("role_storage_provider")),
        )
    }

    // ---- Helpers -------------------------------------------------------------------------

    private fun requireString(name: String): String =
        checkNotNull(userFacingCopy()[name]) { "strings.xml is missing $name" }

    private fun apiError(statusCode: Int, code: String) = NodeApiException(
        statusCode = statusCode,
        protocolVersion = 1,
        code = code,
        retryable = statusCode >= 500,
        message = "Engine-facing detail nobody should read.",
    )

    private fun userFacingCopy(): Map<String, String> = COPY

    private companion object {
        val CONSEQUENCE_WORDS = listOf(
            "cannot", "can not", "not be undone", "deleted", "discarded",
            "stops", "removes", "start", "again",
        )

        val JARGON = listOf(
            // Raw wire tokens.
            "storage_provider", "backup_reader", "backup_writer",
            // Engineer nouns the audit called out.
            "\\bnodes?\\b", "\\breplicas?\\b", "\\bproviders?\\b", "\\bchunks?\\b",
            "\\bloopback\\b", "MagicDNS", "\\bALPN\\b", "\\btombstones?\\b",
            // Implementation details that were leaking into settings copy.
            "\\bRust\\b", "\\bKeystore\\b",
        ).map { Regex(it, RegexOption.IGNORE_CASE) }

        val COPY: Map<String, String> by lazy { parseStrings() }

        fun parseStrings(): Map<String, String> {
            val xml = moduleFile("src/main/res/values/strings.xml").readText()
            val singular = Regex("<string name=\"([^\"]+)\">(.*?)</string>", RegexOption.DOT_MATCHES_ALL)
                .findAll(xml)
                .associate { it.groupValues[1] to it.groupValues[2] }
            val plurals = Regex("<plurals name=\"([^\"]+)\">(.*?)</plurals>", RegexOption.DOT_MATCHES_ALL)
                .findAll(xml)
                .flatMap { plural ->
                    Regex("<item quantity=\"([^\"]+)\">(.*?)</item>", RegexOption.DOT_MATCHES_ALL)
                        .findAll(plural.groupValues[2])
                        .map { "${plural.groupValues[1]}[${it.groupValues[1]}]" to it.groupValues[2] }
                }
                .toMap()
            check(singular.isNotEmpty()) { "No strings parsed from strings.xml" }
            return singular + plurals
        }

        /**
         * Gradle runs unit tests from the module directory, but resolve defensively so the
         * test also works when a runner picks the repository root instead.
         */
        fun moduleFile(relative: String): File {
            val direct = File(relative)
            if (direct.isFile) return direct
            var candidate: File? = File("").absoluteFile
            while (candidate != null) {
                val resolved = File(candidate, "apps/android/app/$relative")
                if (resolved.isFile) return resolved
                candidate = candidate.parentFile
            }
            error("Unable to locate $relative from ${File("").absolutePath}")
        }
    }
}
