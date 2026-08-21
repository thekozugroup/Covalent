package life.michaelwong.covalent.ui

import android.content.Context
import android.util.Log
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
 * Plain-language copy for one of the backup server's own error codes.
 *
 * The vocabulary matches `packaging/web/app.js` deliberately, so the sentence a person
 * reads while setting up in a browser is the sentence they read on the phone. Returns
 * null for a code this build does not know, which the caller turns into an authored
 * generic sentence rather than into whatever text arrived on the wire.
 */
internal fun nodeErrorCodeMessageRes(code: String): Int? = when (code) {
    "insufficient_storage" -> R.string.node_error_insufficient_storage
    "resource_limit" -> R.string.node_error_resource_limit
    "job_paused" -> R.string.node_error_job_paused
    "job_cancelled" -> R.string.node_error_job_cancelled
    "job_active" -> R.string.node_error_job_active
    "job_conflict" -> R.string.node_error_job_conflict
    "job_not_complete" -> R.string.node_error_job_not_complete
    "job_not_found" -> R.string.node_error_job_not_found
    "invalid_job_id" -> R.string.node_error_invalid_job_id
    "node_busy" -> R.string.node_error_node_busy
    "node_state_locked" -> R.string.node_error_node_state_locked
    "archive_processing_timeout" -> R.string.node_error_archive_processing_timeout
    "archive_processing_too_slow" -> R.string.node_error_archive_processing_too_slow
    "confirmation_required" -> R.string.node_error_confirmation_required
    "source_changed" -> R.string.node_error_source_changed
    "source_unreadable" -> R.string.node_error_source_unreadable
    "invalid_authorized_root" -> R.string.node_error_invalid_authorized_root
    "unsafe_restore_path" -> R.string.node_error_unsafe_restore_path
    "restore_conflict" -> R.string.node_error_restore_conflict
    "restore_plan_mismatch" -> R.string.node_error_restore_plan_mismatch
    "restore_plan_not_found" -> R.string.node_error_restore_plan_not_found
    "invalid_restore_plan_id" -> R.string.node_error_invalid_restore_plan_id
    "invalid_restore_execute_request" -> R.string.node_error_invalid_restore_execute_request
    "invalid_streamed_restore_plan" -> R.string.node_error_invalid_streamed_restore_plan
    "invalid_target_inventory" -> R.string.node_error_invalid_target_inventory
    "target_inventory_required" -> R.string.node_error_target_inventory_required
    "target_inventory_not_found" -> R.string.node_error_target_inventory_not_found
    "target_inventory_incomplete" -> R.string.node_error_target_inventory_incomplete
    "target_inventory_digest_mismatch" -> R.string.node_error_target_inventory_digest_mismatch
    "target_inventory_job_mismatch" -> R.string.node_error_target_inventory_job_mismatch
    "target_inventory_offset_mismatch" -> R.string.node_error_target_inventory_offset_mismatch
    "target_inventory_page_mismatch" -> R.string.node_error_target_inventory_page_mismatch
    "backup_corrupt" -> R.string.node_error_backup_corrupt
    "backup_unavailable" -> R.string.node_error_backup_unavailable
    "invalid_archive" -> R.string.node_error_invalid_archive
    "invalid_archive_entry" -> R.string.node_error_invalid_archive_entry
    "invalid_archive_metadata" -> R.string.node_error_invalid_archive_metadata
    "archive_metadata_required" -> R.string.node_error_archive_metadata_required
    "archive_upload_headers_required" -> R.string.node_error_archive_upload_headers_required
    "archive_digest_mismatch" -> R.string.node_error_archive_digest_mismatch
    "duplicate_archive_entry" -> R.string.node_error_duplicate_archive_entry
    "invalid_upload_digest" -> R.string.node_error_invalid_upload_digest
    "invalid_upload_length" -> R.string.node_error_invalid_upload_length
    "invalid_upload_offset" -> R.string.node_error_invalid_upload_offset
    "invitation_unavailable" -> R.string.node_error_invitation_unavailable
    "protocol_incompatible" -> R.string.node_error_protocol_incompatible
    "pairing_endpoint_mismatch" -> R.string.node_error_pairing_endpoint_mismatch
    "peer_endpoint_unavailable" -> R.string.node_error_peer_endpoint_unavailable
    "claim_unavailable" -> R.string.node_error_claim_unavailable
    "claim_code_incorrect" -> R.string.node_error_claim_code_incorrect
    "claim_window_expired" -> R.string.node_error_claim_window_expired
    "claim_window_exhausted" -> R.string.node_error_claim_window_exhausted
    "claim_rate_limited" -> R.string.node_error_claim_rate_limited
    "claim_certificate_unavailable" -> R.string.node_error_claim_certificate_unavailable
    "pairing_peer_unreachable" -> R.string.node_error_pairing_peer_unreachable
    "pairing_rejected" -> R.string.node_error_pairing_rejected
    "provider_binding_mismatch" -> R.string.node_error_provider_binding_mismatch
    "invalid_provider_address" -> R.string.node_error_invalid_provider_address
    "invalid_contract" -> R.string.node_error_invalid_contract
    "invalid_json" -> R.string.node_error_invalid_json
    "invalid_content_type" -> R.string.node_error_invalid_content_type
    "method_not_allowed" -> R.string.node_error_method_not_allowed
    "route_not_found" -> R.string.node_error_route_not_found
    "invalid_page_cursor" -> R.string.node_error_invalid_page_cursor
    "invalid_page_limit" -> R.string.node_error_invalid_page_limit
    "internal_error" -> R.string.node_error_internal_error
    else -> null
}

/**
 * Plain-language copy for a failed request.
 *
 * Every path here produces copy this project wrote. Nothing renders `Throwable.message`:
 * for a JVM or library exception that string is a developer diagnostic ("Expected
 * BEGIN_OBJECT but was STRING"), and for a server response it is text this app did not
 * author and cannot vouch for.
 *
 * Order matters. The backup server's own error code is consulted **before** the HTTP
 * status, because several codes travel on a status that means something else in general:
 * `claim_code_incorrect` arrives as a 401, and without this precedence a mistyped setup
 * code would be reported as "your access token was not accepted" — telling a person to
 * reconnect when what they need is to retype six characters.
 */
internal fun nodeFailureMessage(context: Context, error: Throwable, fallbackRes: Int): String {
    if (error is NodeApiException) {
        nodeErrorCodeMessageRes(error.code)?.let { return context.getString(it) }
    }
    val failure = classifyNodeFailure(error)
    if (failure != NodeFailure.UNKNOWN) return context.getString(nodeFailureMessageRes(failure))
    // Kept out of the person's way, but kept: without it an unmapped failure is
    // undiagnosable from a bug report. The code is safe to log; the body is not shown.
    if (error is NodeApiException) {
        Log.w(LOG_TAG, "Unmapped backup server error code: ${error.code} (${error.statusCode})")
    } else {
        Log.w(LOG_TAG, "Unmapped node failure shown as generic copy", error)
    }
    return context.getString(fallbackRes)
}

private const val LOG_TAG = "CovalentNodeFailure"

private fun causeChain(error: Throwable): List<Throwable> =
    generateSequence(error) { previous -> previous.cause?.takeIf { it !== previous } }
        .take(8)
        .toList()
