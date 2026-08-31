package life.michaelwong.covalent

import java.io.File
import life.michaelwong.covalent.ui.nodeErrorCodeMessageRes
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The backup server's error codes must reach a person as copy this project wrote.
 *
 * Every expectation below is a literal, never a value read back out of the mapping under
 * test: a test that asserted `nodeErrorCodeMessageRes(code) == nodeErrorCodeMessageRes(code)`
 * would survive any change to the mapping, including deleting it.
 */
class NodeErrorCodeCopyTest {

    @Test
    fun theFirstRunClaimCodesAreMappedToTheirOwnSentences() {
        assertEquals(R.string.node_error_claim_unavailable, nodeErrorCodeMessageRes("claim_unavailable"))
        assertEquals(R.string.node_error_claim_code_incorrect, nodeErrorCodeMessageRes("claim_code_incorrect"))
        assertEquals(R.string.node_error_claim_window_expired, nodeErrorCodeMessageRes("claim_window_expired"))
        assertEquals(
            R.string.node_error_claim_window_exhausted,
            nodeErrorCodeMessageRes("claim_window_exhausted"),
        )
        assertEquals(R.string.node_error_claim_rate_limited, nodeErrorCodeMessageRes("claim_rate_limited"))
        assertEquals(
            R.string.node_error_claim_certificate_unavailable,
            nodeErrorCodeMessageRes("claim_certificate_unavailable"),
        )
        assertEquals(
            R.string.node_error_peer_endpoint_unavailable,
            nodeErrorCodeMessageRes("peer_endpoint_unavailable"),
        )
    }

    @Test
    fun theRetiredPairingEndpointCodeIsNotMapped() {
        assertNull(
            "The backup server no longer emits pairing_endpoint_unavailable",
            nodeErrorCodeMessageRes("pairing_endpoint_unavailable"),
        )
    }

    @Test
    fun anUnknownCodeIsNotMappedSoTheCallerUsesAuthoredCopy() {
        assertNull(nodeErrorCodeMessageRes("a_code_from_a_newer_backup_server"))
        assertNull(nodeErrorCodeMessageRes(""))
    }

    /**
     * These are the first words a new person reads from Covalent, so each one has to say
     * what happened and what to do next rather than only naming the failure.
     */
    @Test
    fun theFirstRunCopyExplainsWhatToDoNext() {
        val copy = strings()
        fun sentence(code: String): String =
            checkNotNull(copy["node_error_$code"]) { "strings.xml is missing node_error_$code" }

        assertTrue(
            "An incorrect claim must direct the owner to the trusted CLI: " +
                sentence("claim_code_incorrect"),
            sentence("claim_code_incorrect").contains("covalent claim"),
        )
        assertTrue(
            "Android must say that it never accepts setup codes: " +
                sentence("claim_code_incorrect"),
            sentence("claim_code_incorrect").contains("this phone never accepts setup codes"),
        )
        assertTrue(
            "An expired code must say a new one can be minted: ${sentence("claim_window_expired")}",
            sentence("claim_window_expired").contains("Restart Covalent"),
        )
        assertTrue(
            "A spent single-use code may mean somebody else claimed the server, and must " +
                "say so: ${sentence("claim_window_exhausted")}",
            sentence("claim_window_exhausted").contains("someone else"),
        )
        assertTrue(
            "A missing peer address must point at the setting: " +
                sentence("peer_endpoint_unavailable"),
            sentence("peer_endpoint_unavailable").contains("settings"),
        )
        assertFalse(
            "Android setup must not direct people to protected server state",
            checkNotNull(copy["caddy_ca_guidance"]).contains("/config/"),
        )
        assertTrue(
            "Android setup must use the trusted CLI claim output",
            checkNotNull(copy["caddy_ca_guidance"]).contains("covalent claim"),
        )
        // Everything a person can act on says what happened and then what to do, so it
        // carries at least two sentences. claim_unavailable is deliberately excluded:
        // a server that already has an owner cannot be claimed by anybody, and inventing
        // a next step for it would be a lie.
        listOf(
            "claim_code_incorrect",
            "claim_window_expired",
            "claim_window_exhausted",
            "claim_rate_limited",
            "claim_certificate_unavailable",
            "peer_endpoint_unavailable",
        ).forEach { code ->
            val text = sentence(code)
            assertTrue(
                "node_error_$code must say what happened and then what to do: $text",
                text.contains(". "),
            )
        }
        listOf(
            "claim_unavailable",
            "claim_code_incorrect",
            "claim_window_expired",
            "claim_window_exhausted",
            "claim_rate_limited",
            "claim_certificate_unavailable",
            "peer_endpoint_unavailable",
        ).forEach { code ->
            val text = sentence(code)
            assertTrue("node_error_$code must be a finished sentence: $text", text.endsWith("."))
            assertFalse("node_error_$code must not expose an exception: $text", text.contains("Exception"))
        }
    }

    @Test
    fun everyMappedCodeResolvesToItsOwnStringAndNoneIsShared() {
        val declared = declaredCodes()
        assertTrue("The code table should not have shrunk to nothing", declared.size > 50)
        val resources = declared.associateWith { code ->
            assertNotNull("node_error_$code is declared but not mapped", nodeErrorCodeMessageRes(code))
            checkNotNull(nodeErrorCodeMessageRes(code))
        }
        assertEquals(
            "Two backup server codes must never share one sentence",
            resources.size,
            resources.values.distinct().size,
        )
    }

    /**
     * The regression this whole file exists for: nothing in the failure path may render a
     * value that came off the wire or out of a JVM exception.
     */
    @Test
    fun theFailurePathNeverRendersAThrowableMessage() {
        val source = moduleFile("src/main/java/life/michaelwong/covalent/ui/NodeFailure.kt").readText()
        val renderingLines = source.lines()
            .filterNot { it.trimStart().startsWith("*") || it.trimStart().startsWith("//") }
            .filter { it.contains("error.message") || it.contains("it.message") }
        assertTrue(
            "NodeFailure must not read a Throwable's message: $renderingLines",
            renderingLines.isEmpty(),
        )
        assertTrue(
            "The backup server's error code must be consulted before the HTTP status, or " +
                "claim_code_incorrect (a 401) is reported as a rejected access token",
            source.indexOf("nodeErrorCodeMessageRes(error.code)") <
                source.indexOf("val failure = classifyNodeFailure(error)"),
        )
    }

    private fun declaredCodes(): List<String> =
        Regex("<string name=\"node_error_([a-z0-9_]+)\">")
            .findAll(moduleFile("src/main/res/values/strings.xml").readText())
            .map { it.groupValues[1] }
            .toList()

    private fun strings(): Map<String, String> =
        Regex("<string name=\"([^\"]+)\">(.*?)</string>", RegexOption.DOT_MATCHES_ALL)
            .findAll(moduleFile("src/main/res/values/strings.xml").readText())
            .associate { it.groupValues[1] to it.groupValues[2] }

    private companion object {
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
