package life.michaelwong.covalent

import androidx.test.platform.app.InstrumentationRegistry
import java.io.IOException
import life.michaelwong.covalent.data.NodeApiException
import life.michaelwong.covalent.ui.nodeFailureMessage
import org.json.JSONException
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Test

/**
 * End-to-end copy for a failed request, resolved through a real [android.content.Context].
 *
 * The unit tests pin the mapping table; these pin what a person actually reads.
 */
class NodeFailureCopyTest {
    private val context = InstrumentationRegistry.getInstrumentation().targetContext

    @Test
    fun aLibraryExceptionNeverReachesTheScreen() {
        val leak = "Expected BEGIN_OBJECT but was STRING at line 1 column 2 path \$.id"
        val shown = nodeFailureMessage(
            context,
            JSONException(leak),
            R.string.error_node_action_failed,
        )
        assertFalse("The parser's message reached the screen: $shown", shown.contains("BEGIN_OBJECT"))
        assertEquals(context.getString(R.string.error_node_action_failed), shown)
    }

    @Test
    fun anUnmappedServerCodeFallsBackToAuthoredCopyRatherThanTheResponseBody() {
        val shown = nodeFailureMessage(
            context,
            NodeApiException(
                statusCode = 400,
                protocolVersion = 1,
                code = "a_code_from_a_newer_backup_server",
                retryable = false,
                message = "Engine-facing detail nobody should read.",
            ),
            R.string.error_node_action_failed,
        )
        assertFalse("The response body reached the screen: $shown", shown.contains("Engine-facing"))
        assertEquals(context.getString(R.string.error_node_action_failed), shown)
    }

    /**
     * `claim_code_incorrect` travels on a 401. Without code-before-status precedence an
     * owner-claim failure is reported as a rejected access token, which sends a person to
     * reconnect instead of returning to the trusted CLI-only claim flow.
     */
    @Test
    fun aMistypedSetupCodeIsNotReportedAsARejectedAccessToken() {
        val shown = nodeFailureMessage(
            context,
            NodeApiException(
                statusCode = 401,
                protocolVersion = 1,
                code = "claim_code_incorrect",
                retryable = false,
                message = "unauthorized",
            ),
            R.string.error_node_action_failed,
        )
        assertEquals(context.getString(R.string.node_error_claim_code_incorrect), shown)
        assertFalse(
            "An owner-claim failure must not be reported as a token problem: $shown",
            shown == context.getString(R.string.error_server_needs_reconnect),
        )
    }

    @Test
    fun aStillPreparingCertificateIsNotReportedAsAServerCrash() {
        val shown = nodeFailureMessage(
            context,
            NodeApiException(
                statusCode = 503,
                protocolVersion = 1,
                code = "claim_certificate_unavailable",
                retryable = true,
                message = "unavailable",
            ),
            R.string.error_node_action_failed,
        )
        assertEquals(context.getString(R.string.node_error_claim_certificate_unavailable), shown)
    }

    @Test
    fun transportFailuresStillGetTheirOwnCopy() {
        val shown = nodeFailureMessage(
            context,
            IOException("ECONNREFUSED (Connection refused)"),
            R.string.error_node_action_failed,
        )
        assertEquals(context.getString(R.string.error_server_unreachable), shown)
        assertFalse(shown.contains("ECONNREFUSED"))
    }
}
