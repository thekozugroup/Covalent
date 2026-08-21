package life.michaelwong.covalent.ui

import android.content.Context
import java.io.IOException
import java.security.GeneralSecurityException
import javax.net.ssl.SSLException
import life.michaelwong.covalent.R
import life.michaelwong.covalent.data.NodeApiException
import life.michaelwong.covalent.data.NodeProtocolException

/**
 * Transport- and protocol-level failures that mean the same thing to a person no matter
 * which screen produced them. Anything outside this set keeps the backup server's own
 * message, which is domain-specific and usually more useful than a generic sentence.
 */
internal enum class NodeFailure {
    UNREACHABLE,
    UNTRUSTED_CERTIFICATE,
    NEEDS_RECONNECT,
    NOT_PERMITTED,
    UNSUPPORTED_BY_SERVER,
    SERVER_PROBLEM,
    UNKNOWN,
}

internal fun classifyNodeFailure(error: Throwable): NodeFailure {
    if (error is NodeProtocolException) return NodeFailure.UNSUPPORTED_BY_SERVER
    if (error is NodeApiException) {
        return when {
            error.statusCode == 401 -> NodeFailure.NEEDS_RECONNECT
            error.statusCode == 403 -> NodeFailure.NOT_PERMITTED
            error.statusCode == 404 || error.statusCode == 405 || error.statusCode == 501 ->
                NodeFailure.UNSUPPORTED_BY_SERVER
            error.statusCode >= 500 -> NodeFailure.SERVER_PROBLEM
            else -> NodeFailure.UNKNOWN
        }
    }
    val chain = causeChain(error)
    // SSLException extends IOException, so trust failures must be recognised first.
    if (chain.any { it is SSLException || it is GeneralSecurityException }) {
        return NodeFailure.UNTRUSTED_CERTIFICATE
    }
    if (chain.any { it is IOException }) return NodeFailure.UNREACHABLE
    return NodeFailure.UNKNOWN
}

internal fun nodeFailureMessageRes(failure: NodeFailure): Int = when (failure) {
    NodeFailure.UNREACHABLE -> R.string.error_server_unreachable
    NodeFailure.UNTRUSTED_CERTIFICATE -> R.string.error_server_certificate
    NodeFailure.NEEDS_RECONNECT -> R.string.error_server_needs_reconnect
    NodeFailure.NOT_PERMITTED -> R.string.error_server_not_permitted
    NodeFailure.UNSUPPORTED_BY_SERVER -> R.string.error_server_unsupported
    NodeFailure.SERVER_PROBLEM -> R.string.error_server_problem
    NodeFailure.UNKNOWN -> R.string.error_node_action_failed
}

/**
 * Plain-language copy for a failed request. A recognised transport or protocol failure is
 * replaced outright; anything else falls back to the server's own message, and only then
 * to the generic sentence.
 */
internal fun nodeFailureMessage(context: Context, error: Throwable, fallbackRes: Int): String {
    val failure = classifyNodeFailure(error)
    if (failure != NodeFailure.UNKNOWN) return context.getString(nodeFailureMessageRes(failure))
    return error.message?.takeIf(String::isNotBlank) ?: context.getString(fallbackRes)
}

private fun causeChain(error: Throwable): List<Throwable> =
    generateSequence(error) { previous -> previous.cause?.takeIf { it !== previous } }
        .take(8)
        .toList()
