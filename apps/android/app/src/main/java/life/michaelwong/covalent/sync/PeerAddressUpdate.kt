package life.michaelwong.covalent.sync

import java.net.InetAddress
import kotlinx.coroutines.CancellationException

internal const val MAX_PEER_ADDRESS_CHARS = 128

/** Exact optimistic-concurrency request captured from one authenticated status row. */
internal data class PeerAddressRefreshRequest(
    val peerId: String,
    val expectedAddress: String,
    val candidateAddress: String,
)

/**
 * Immutable editor state. A failed request remains byte-for-byte available for a deliberate retry.
 * Editing either the peer or candidate creates a new draft and discards that captured request.
 */
internal data class PeerAddressUpdateDraft(
    val peerId: String,
    val displayName: String,
    val savedAddress: String,
    val candidateAddress: String = "",
    val pendingRequest: PeerAddressRefreshRequest? = null,
) {
    init {
        require(savedAddress.length in 1..MAX_PEER_ADDRESS_CHARS && savedAddress.none(Char::isISOControl))
        require(candidateAddress.length <= MAX_PEER_ADDRESS_CHARS && candidateAddress.none(Char::isISOControl))
        pendingRequest?.let {
            require(it.peerId == peerId && it.expectedAddress == savedAddress && it.candidateAddress == candidateAddress)
        }
    }

    fun edit(candidate: String): PeerAddressUpdateDraft? =
        candidate.takeIf { it.length <= MAX_PEER_ADDRESS_CHARS && it.none(Char::isISOControl) }
            ?.let { copy(candidateAddress = it, pendingRequest = null) }

    fun capture(): PeerAddressUpdateDraft {
        if (pendingRequest != null) return this
        val candidate = requireNumericPeerAddress(candidateAddress)
        require(candidate != savedAddress) { "Enter a new device address." }
        return copy(
            candidateAddress = candidate,
            pendingRequest = PeerAddressRefreshRequest(peerId, savedAddress, candidate),
        )
    }

    /** A stale optimistic-concurrency request requires a fresh status row and user selection. */
    fun afterFailure(code: String?): PeerAddressUpdateDraft? =
        takeUnless { code == "peer_address_changed" }

    /**
     * Keep editing while status has the captured old address, or the candidate after an
     * ambiguous response. The latter must retain the old expected address: the node uses
     * that exact retry to finish a durably started address transition.
     */
    fun afterStatus(statusPeerId: String?, statusAddress: String?): PeerAddressUpdateDraft? =
        takeIf {
            statusPeerId == peerId &&
                (statusAddress == savedAddress || pendingRequest?.candidateAddress == statusAddress)
        }
}

/** Awaited follow-up from one exact request; none of these variants claims peer connectivity. */
internal sealed interface PeerAddressUpdateOutcome<out T> {
    data class Updated<T>(
        val freshStatus: T?,
        val statusFailure: Exception?,
        val providerFailure: Exception?,
    ) : PeerAddressUpdateOutcome<T>

    data class Changed<T>(val freshStatus: T?) : PeerAddressUpdateOutcome<T>
    data class Failed(val draft: PeerAddressUpdateDraft, val failure: Exception) : PeerAddressUpdateOutcome<Nothing>
}

/**
 * Serialize the mutation and its authoritative follow-up. A successful mutation is durable even
 * when either refresh fails, while a changed expected address requires a fresh status selection.
 */
internal suspend fun <T> completePeerAddressUpdate(
    captured: PeerAddressUpdateDraft,
    submit: suspend (PeerAddressRefreshRequest) -> Unit,
    reloadStatus: suspend () -> T,
    refreshProviders: suspend () -> Unit,
    errorCode: (Exception) -> String?,
): PeerAddressUpdateOutcome<T> {
    val request = checkNotNull(captured.pendingRequest)
    try {
        submit(request)
    } catch (failure: Exception) {
        if (failure is CancellationException) throw failure
        if (errorCode(failure) != "peer_address_changed") {
            return PeerAddressUpdateOutcome.Failed(captured, failure)
        }
        val freshStatus = try {
            reloadStatus()
        } catch (refreshFailure: Exception) {
            if (refreshFailure is CancellationException) throw refreshFailure
            null
        }
        return PeerAddressUpdateOutcome.Changed(freshStatus)
    }

    var statusFailure: Exception? = null
    val freshStatus = try {
        reloadStatus()
    } catch (failure: Exception) {
        if (failure is CancellationException) throw failure
        statusFailure = failure
        null
    }
    var providerFailure: Exception? = null
    try {
        refreshProviders()
    } catch (failure: Exception) {
        if (failure is CancellationException) throw failure
        providerFailure = failure
    }
    return PeerAddressUpdateOutcome.Updated(freshStatus, statusFailure, providerFailure)
}

/** Validate the user-facing shape without DNS. The Rust endpoint remains canonical authority. */
internal fun requireNumericPeerAddress(raw: String): String {
    require(raw.length in 1..MAX_PEER_ADDRESS_CHARS && raw.none(Char::isISOControl)) {
        "Enter a numeric IP address and port."
    }
    val host: String
    val portText: String
    if (raw.startsWith('[')) {
        val close = raw.indexOf(']')
        require(close > 1 && close + 1 < raw.length && raw[close + 1] == ':') {
            "Enter a numeric IP address and port."
        }
        host = raw.substring(1, close)
        portText = raw.substring(close + 2)
        require(host.contains(':') && '%' !in host) { "Enter a numeric IP address and port." }
        val parsed = runCatching { InetAddress.getByName(host) }.getOrNull()
        require(
            parsed != null &&
                !parsed.isAnyLocalAddress &&
                !parsed.isMulticastAddress &&
                !parsed.isLinkLocalAddress,
        ) { "Enter a numeric IP address and port." }
    } else {
        val separator = raw.lastIndexOf(':')
        require(separator > 0 && separator == raw.indexOf(':')) { "Enter a numeric IP address and port." }
        host = raw.substring(0, separator)
        portText = raw.substring(separator + 1)
        val octets = host.split('.')
        require(octets.size == 4 && octets.all { part ->
            part.isNotEmpty() && part.all(Char::isDigit) &&
                part.toIntOrNull()?.let { it in 0..255 && it.toString() == part } == true
        }) { "Enter a numeric IP address and port." }
        val values = octets.map(String::toInt)
        require(values.any { it != 0 } && values != listOf(255, 255, 255, 255) && values[0] !in 224..239) {
            "Enter a numeric IP address and port."
        }
    }
    val port = portText.toIntOrNull()
    require(port != null && port in 1..65_535 && port.toString() == portText) {
        "Enter a numeric IP address and port."
    }
    return raw
}
