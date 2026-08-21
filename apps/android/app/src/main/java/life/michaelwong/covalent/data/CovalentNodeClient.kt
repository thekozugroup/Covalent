package life.michaelwong.covalent.data

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import androidx.core.content.edit
import java.net.HttpURLConnection
import java.net.URI
import java.net.URL
import java.lang.reflect.Proxy
import java.nio.ByteBuffer
import java.security.KeyStore
import java.security.MessageDigest
import java.security.cert.CertificateException
import java.security.cert.CertificateFactory
import java.security.cert.X509Certificate
import java.util.UUID
import java.util.Base64 as JBase64
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec
import javax.net.ssl.HttpsURLConnection
import javax.net.ssl.SSLContext
import javax.net.ssl.TrustManagerFactory
import javax.net.ssl.X509TrustManager
import org.json.JSONArray
import org.json.JSONObject
import life.michaelwong.covalent.model.DiscoveryCandidate
import life.michaelwong.covalent.model.NodeStatus
import life.michaelwong.covalent.model.NetworkPairing
import life.michaelwong.covalent.model.NetworkPairingDirection
import life.michaelwong.covalent.model.NetworkPairingState
import life.michaelwong.covalent.model.PeerGrant
import life.michaelwong.covalent.model.PeerTransport
import life.michaelwong.covalent.model.Provider
import life.michaelwong.covalent.model.ProviderReachability
import life.michaelwong.covalent.model.RememberedBackup
import life.michaelwong.covalent.model.RestorePlanPage
import life.michaelwong.covalent.model.RestorePlanReference
import life.michaelwong.covalent.model.RestorePreviewEntry
import life.michaelwong.covalent.model.TargetInventoryBinding
import life.michaelwong.covalent.model.TargetInventoryDraft
import life.michaelwong.covalent.model.TargetInventoryEntry
import life.michaelwong.covalent.model.TargetInventoryReference
import life.michaelwong.covalent.model.TransferKind
import life.michaelwong.covalent.model.TransferRecord
import life.michaelwong.covalent.model.TransferState
import life.michaelwong.covalent.model.TransportIdentity

data class EnrolledTrust(
    val caCertificateDerBase64: String? = null,
    val sha256Pin: String? = null,
) {
    init {
        require(caCertificateDerBase64.isNullOrBlank() || sha256Pin.isNullOrBlank()) {
            "Enroll either an exact CA certificate or a SHA-256 pin, not both."
        }
    }
}

/** A small, contract-first client for the authenticated local Covalent node API. */
class CovalentNodeClient(
    private val trustProvider: () -> EnrolledTrust? = { null },
) {
    fun status(baseUrl: String): NodeStatus {
        val json = request(baseUrl, "GET", "/api/v1/status", null, null).body
        val protocolVersion = json.getInt("protocolVersion")
        if (protocolVersion != COVALENT_PROTOCOL_VERSION) {
            throw NodeProtocolException(protocolVersion)
        }
        return NodeStatus(
            deviceName = json.getString("deviceName"),
            protocolVersion = protocolVersion.toUShort(),
            lanDiscovery = json.getBoolean("lanDiscovery"),
            platformTier = when (json.getString("platformTier")) {
                "tier1" -> life.michaelwong.covalent.model.PlatformTier.TIER_1
                "tier2" -> life.michaelwong.covalent.model.PlatformTier.TIER_2
                else -> throw IllegalStateException("Node returned an unknown platform tier.")
            },
            state = json.getString("state"),
        )
    }

    fun discovery(baseUrl: String, token: String): List<DiscoveryCandidate> {
        val values = request(baseUrl, "GET", "/api/v1/discovery", token, null).array
        return List(values.length()) { index ->
            values.getJSONObject(index).let {
                DiscoveryCandidate(it.getString("source"), it.getString("endpoint"), it.getString("serviceId"))
            }
        }
    }

    fun providers(baseUrl: String, token: String): List<Provider> {
        val grants = peerGrants(baseUrl, token).associateBy(PeerGrant::peerDeviceId)
        val values = request(baseUrl, "GET", "/api/v1/providers", token, null).array
        return List(values.length()) { index ->
            values.getJSONObject(index).let {
                val peerId = it.getString("peerId")
                val grant = grants[peerId]
                Provider(
                    peerId = peerId,
                    address = it.getString("address"),
                    fingerprint = it.getString("certificateFingerprint"),
                    displayName = grant?.displayName,
                    roles = grant?.roles.orEmpty(),
                    // The current API lists remembered connections but exposes no live probe or capacity.
                    reachability = ProviderReachability.UNKNOWN,
                    capacityBytes = null,
                )
            }
        }
    }

    fun transportIdentity(baseUrl: String, token: String): TransportIdentity {
        val json = request(baseUrl, "GET", "/api/v1/transport/identity", token, null).body
        return TransportIdentity(
            deviceId = json.getString("deviceId"),
            peerPort = json.getInt("peerPort"),
            certificateDer = json.getString("certificateDer"),
            certificateFingerprint = json.getString("certificateFingerprint"),
        )
    }

    fun peerGrants(baseUrl: String, token: String): List<PeerGrant> {
        val roster = request(baseUrl, "GET", "/api/v1/rosters/current", token, null).nullableBody
            ?: return emptyList()
        val values = roster.optJSONArray("grants") ?: return emptyList()
        return List(values.length()) { index ->
            values.getJSONObject(index).let { value ->
                val roles = value.getJSONArray("roles")
                PeerGrant(
                    peerDeviceId = value.getString("peerDeviceId"),
                    displayName = value.getString("displayName"),
                    roles = buildSet { repeat(roles.length()) { add(roles.getString(it)) } },
                    revoked = value.getBoolean("revoked"),
                )
            }
        }
    }

    fun connectProvider(
        baseUrl: String,
        token: String,
        peerTransport: PeerTransport,
    ): Provider {
        val json = post(
            baseUrl,
            token,
            "/api/v1/providers/connect",
            peerTransportConnectPayload(peerTransport),
        )
        return Provider(
            peerId = json.getString("peerId"),
            address = json.getString("address"),
            fingerprint = json.getString("certificateFingerprint"),
            reachability = ProviderReachability.CONNECTED,
        )
    }

    fun startNetworkPairing(baseUrl: String, token: String, candidateAddress: String): NetworkPairing {
        requireNetworkPairingAddress(candidateAddress)
        return post(baseUrl, token, "/api/v1/pair/network/start", JSONObject()
            .put("candidateAddress", candidateAddress)).toNetworkPairing()
    }

    fun pendingNetworkPairings(baseUrl: String, token: String): List<NetworkPairing> {
        val values = request(baseUrl, "GET", "/api/v1/pair/network/pending", token, null).array
        return List(values.length()) { values.getJSONObject(it).toNetworkPairing() }
    }

    fun confirmNetworkPairing(
        baseUrl: String,
        token: String,
        pairingId: String,
        displayedCode: String,
    ): NetworkPairing {
        requireNetworkPairingId(pairingId)
        require(displayedCode.matches(SAFE_AUTHENTICATION_STRING)) { "The pairing confirmation code is invalid." }
        return post(baseUrl, token, "/api/v1/pair/network/$pairingId/confirm", JSONObject()
            .put("displayedCode", displayedCode)).toNetworkPairing()
    }

    fun cancelNetworkPairing(baseUrl: String, token: String, pairingId: String) {
        requireNetworkPairingId(pairingId)
        request(baseUrl, "DELETE", "/api/v1/pair/network/$pairingId", token, null)
    }

    fun controlJob(baseUrl: String, token: String, jobId: String, action: String): String =
        post(
            baseUrl,
            token,
            "/api/v1/jobs/control",
            JSONObject().put("jobId", jobId).put("action", action),
        ).getString("state")

    fun acknowledgeJob(baseUrl: String, token: String, jobId: String) {
        postNoContent(
            baseUrl,
            token,
            "/api/v1/jobs/acknowledge",
            JSONObject().put("jobId", jobId),
        )
    }

    fun discardJob(baseUrl: String, token: String, jobId: String) {
        postNoContent(
            baseUrl,
            token,
            "/api/v1/jobs/discard",
            JSONObject().put("jobId", jobId),
        )
    }

    fun backups(baseUrl: String, token: String): List<RememberedBackup> {
        val values = request(baseUrl, "GET", "/api/v1/backups", token, null).array
        return List(values.length()) { index ->
            values.getJSONObject(index).let { value ->
                val providers = value.getJSONArray("selectedProviderIds")
                RememberedBackup(
                    backupId = value.getString("backupId"),
                    name = value.getString("name"),
                    ownerDeviceId = value.getString("ownerDeviceId"),
                    latestSnapshotId = value.optionalString("latestSnapshotId"),
                    latestCommittedAtUnixMs = value.optionalLong("latestCommittedAtUnixMs"),
                    snapshotCount = value.getLong("snapshotCount"),
                    selectedProviderIds = buildSet {
                        repeat(providers.length()) { add(providers.getString(it)) }
                    },
                )
            }
        }
    }

    /** Uploads one immutable, bounded target inventory without exceeding normal JSON limits. */
    fun uploadTargetInventory(
        baseUrl: String,
        token: String,
        jobId: String,
        draft: TargetInventoryDraft,
        pageSize: Int = TARGET_INVENTORY_PAGE_SIZE,
    ): TargetInventoryReference {
        require(pageSize in 1..MAX_TARGET_INVENTORY_PAGE_SIZE) {
            "Target inventory pages must contain between 1 and 5,000 entries."
        }
        validateTargetInventoryDraft(draft)
        val started = post(
            baseUrl,
            token,
            "/api/v1/restores/archive/inventories",
            JSONObject()
                .put("jobId", jobId)
                .put("schemaVersion", 1)
                .put("rootIdentity", draft.rootIdentity)
                .put("entryCount", draft.entries.size)
                .put("totalBytes", draft.totalBytes),
        ).toTargetInventoryUpload(jobId)
        check(started.nextOffset == 0L) { "The node returned a nonzero target inventory start offset." }

        var offset = 0
        while (offset < draft.entries.size) {
            val end = minOf(offset + pageSize, draft.entries.size)
            val entries = draft.entries.subList(offset, end)
            val payload = JSONObject()
                .put("jobId", jobId)
                .put("offset", offset)
                .put("pageDigest", targetInventoryPageDigest(entries))
                .put("entries", JSONArray().apply { entries.forEach { put(it.toJson()) } })
            val response = try {
                post(
                    baseUrl,
                    token,
                    "/api/v1/restores/archive/inventories/${started.inventoryId}/pages",
                    payload,
                ).toTargetInventoryUpload(jobId, started.inventoryId)
            } catch (error: NodeApiException) {
                val authoritative = error.inventoryOffset
                if (error.code != "target_inventory_offset_mismatch" ||
                    authoritative == null || authoritative !in 0..draft.entries.size.toLong()
                ) {
                    throw error
                }
                offset = authoritative.toInt()
                continue
            }
            check(response.nextOffset == end.toLong()) {
                "The node returned a different target inventory page offset."
            }
            offset = end
        }

        return post(
            baseUrl,
            token,
            "/api/v1/restores/archive/inventories/${started.inventoryId}/finalize",
            JSONObject()
                .put("jobId", jobId)
                .put("entryCount", draft.entries.size)
                .put("totalBytes", draft.totalBytes)
                .put("inventoryDigest", ""),
        ).toTargetInventoryReference(jobId, started.inventoryId, draft)
    }

    /** Creates a no-write restore plan and immediately fetches only its first bounded page. */
    fun previewArchiveRestore(
        baseUrl: String,
        token: String,
        payload: JSONObject,
        pageSize: Int = RESTORE_PREVIEW_PAGE_SIZE,
    ): RestorePlanPage {
        require(pageSize in 1..MAX_RESTORE_PREVIEW_PAGE_SIZE) {
            "Restore preview pages must contain between 1 and 1,000 entries."
        }
        val response = post(baseUrl, token, "/api/v1/restores/archive/preview", payload)
        val reference = response.toRestorePlanReference()
        return if (reference.planId == null) {
            legacyRestorePlanPage(reference, cursor = null, pageSize)
        } else {
            restorePlanPage(baseUrl, token, reference, cursor = null, pageSize)
        }
    }

    /** Fetches one bounded page while verifying it still belongs to the selected signed plan. */
    fun restorePlanPage(
        baseUrl: String,
        token: String,
        reference: RestorePlanReference,
        cursor: String?,
        limit: Int = RESTORE_PREVIEW_PAGE_SIZE,
    ): RestorePlanPage {
        require(limit in 1..MAX_RESTORE_PREVIEW_PAGE_SIZE) {
            "Restore preview pages must contain between 1 and 1,000 entries."
        }
        if (reference.planId == null) return legacyRestorePlanPage(reference, cursor, limit)
        require(reference.planId.matches(SAFE_PLAN_ID)) { "The restore plan ID is invalid." }
        val parsedCursor = cursor?.also {
            require(it.isNotEmpty() && it.all(Char::isDigit)) { "The restore plan cursor is invalid." }
        }
        val path = buildString {
            append("/api/v1/restores/plans/")
            append(reference.planId)
            append("?limit=")
            append(limit)
            parsedCursor?.let { append("&cursor=").append(it) }
        }
        val page = request(baseUrl, "GET", path, token, null).body
        val pageReference = page.toRestorePlanReference()
        check(sameSignedRestorePlan(pageReference, reference)) {
            "The node returned a different signed restore plan."
        }
        return page.toRestorePlanPage(pageReference).also {
            check(it.entryOffset == (parsedCursor?.toLong() ?: 0L)) {
                "The node returned a different restore preview page."
            }
        }
    }

    /** Materializes bounded pages while keeping the signed reference exact across every page. */
    fun restorePlanEntries(
        baseUrl: String,
        token: String,
        firstPage: RestorePlanPage,
        pageSize: Int = MAX_RESTORE_PREVIEW_PAGE_SIZE,
    ): List<RestorePreviewEntry> {
        val entries = firstPage.entries.toMutableList()
        var page = firstPage
        while (page.nextCursor != null) {
            page = restorePlanPage(baseUrl, token, firstPage.reference, page.nextCursor, pageSize)
            entries += page.entries
        }
        check(entries.size.toLong() == firstPage.reference.totalEntries) {
            "The node omitted part of the signed restore plan."
        }
        check(entries.map(RestorePreviewEntry::destinationPath).toSet().size == entries.size) {
            "The signed restore plan contains duplicate destination paths."
        }
        return entries
    }

    fun post(baseUrl: String, token: String, path: String, payload: JSONObject): JSONObject =
        request(baseUrl, "POST", path, token, payload.toString()).body

    fun postNoContent(baseUrl: String, token: String, path: String, payload: JSONObject) {
        request(baseUrl, "POST", path, token, payload.toString())
    }

    private fun request(
        baseUrl: String,
        method: String,
        path: String,
        token: String?,
        payload: String?,
    ): NodeResponse {
        val connection = openConnection(baseUrl, path, method, token, "application/json").apply {
            if (payload != null) {
                doOutput = true
                setRequestProperty("Content-Type", "application/json")
                val bytes = payload.encodeToByteArray()
                setFixedLengthStreamingMode(bytes.size)
                outputStream.use { it.write(bytes) }
            }
        }
        return NodeResponse(readResponse(connection))
    }

    internal fun openConnection(
        baseUrl: String,
        path: String,
        method: String,
        token: String?,
        accept: String,
        readTimeoutMillis: Int = 30_000,
    ): HttpURLConnection {
        val normalized = baseUrl.trim().removeSuffix("/")
        val endpoint = runCatching { URI(normalized) }.getOrElse {
            throw IllegalArgumentException("Use a complete Covalent node URL.")
        }
        val scheme = endpoint.scheme?.lowercase()
        require(
            scheme in setOf("http", "https") && !endpoint.host.isNullOrBlank() && endpoint.userInfo == null &&
                (endpoint.rawPath?.takeIf(String::isNotEmpty) ?: "/") == "/" &&
                endpoint.rawQuery == null && endpoint.rawFragment == null &&
                (endpoint.port == -1 || endpoint.port in 1..65_535),
        ) {
            "Use a complete HTTP or HTTPS Covalent node URL."
        }
        val url = runCatching { URL(normalized + path) }.getOrElse {
            throw IllegalArgumentException("Use a complete Covalent node URL.")
        }
        if (!token.isNullOrBlank() && url.protocol != "https" && !url.isLoopback()) {
            throw IllegalArgumentException(
                "Covalent sends its API token only over HTTPS or device loopback. Configure HTTPS for a network node.",
            )
        }
        return (url.openConnection() as HttpURLConnection).apply {
            if (this is HttpsURLConnection) configureEnrolledTrust(trustProvider())
            requestMethod = method
            connectTimeout = 8_000
            readTimeout = readTimeoutMillis
            useCaches = false
            setRequestProperty("Accept", accept)
            setRequestProperty("Cache-Control", "no-store")
            token?.takeIf { it.isNotBlank() }?.let {
                setRequestProperty("Authorization", "Bearer $it")
            }
        }
    }

    private fun HttpsURLConnection.configureEnrolledTrust(trust: EnrolledTrust?) {
        if (trust == null) return
        val trustManager = when {
            !trust.caCertificateDerBase64.isNullOrBlank() -> trustManagerForCertificate(
                decodeCertificate(trust.caCertificateDerBase64),
            )
            !trust.sha256Pin.isNullOrBlank() -> pinnedTrustManager(normalizeSha256Pin(trust.sha256Pin))
            else -> return
        }
        sslSocketFactory = SSLContext.getInstance("TLS").apply {
            init(null, arrayOf(trustManager), null)
        }.socketFactory
        // Keep platform RFC 2818 hostname/SAN verification. Enrolled trust never bypasses it.
        hostnameVerifier = HttpsURLConnection.getDefaultHostnameVerifier()
    }

    internal fun readResponse(connection: HttpURLConnection): String {
        val code = connection.responseCode
        val uploadOffset = connection.getHeaderField("X-Covalent-Upload-Offset")?.toLongOrNull()
        val inventoryOffset = connection.getHeaderField("X-Covalent-Inventory-Offset")?.toLongOrNull()
        val stream = if (code in 200..299) connection.inputStream else connection.errorStream
        val text = stream?.bufferedReader()?.use { it.readText() }.orEmpty()
        connection.disconnect()
        if (code !in 200..299) {
            val payload = runCatching { JSONObject(text) }.getOrNull()
            val protocolVersion = payload?.optInt("protocolVersion", COVALENT_PROTOCOL_VERSION)
                ?: COVALENT_PROTOCOL_VERSION
            if (payload != null && protocolVersion != COVALENT_PROTOCOL_VERSION) {
                throw NodeProtocolException(protocolVersion)
            }
            throw NodeApiException(
                statusCode = code,
                protocolVersion = protocolVersion,
                code = payload?.optString("code").orEmpty().ifBlank { "http_$code" },
                retryable = payload?.optBoolean("retryable", code >= 500) ?: (code >= 500),
                message = payload?.optString("message").orEmpty().ifBlank { "Node returned HTTP $code." },
                uploadOffset = uploadOffset,
                inventoryOffset = inventoryOffset,
            )
        }
        return text
    }

    internal fun ensureSuccess(connection: HttpURLConnection) {
        if (connection.responseCode !in 200..299) {
            readResponse(connection)
            error("The node returned an unsuccessful response.")
        }
    }
}

internal fun peerTransportConnectPayload(peerTransport: PeerTransport): JSONObject = JSONObject()
    .put("peerTransport", JSONObject()
        .put("peerId", peerTransport.peerId)
        .put("displayName", peerTransport.displayName)
        .put("address", peerTransport.address)
        .put("certificateDer", peerTransport.certificateDer)
        .put("certificateFingerprint", peerTransport.certificateFingerprint))

private const val COVALENT_PROTOCOL_VERSION = 1
private const val RESTORE_PREVIEW_PAGE_SIZE = 100
private const val MAX_RESTORE_PREVIEW_PAGE_SIZE = 1_000
private const val TARGET_INVENTORY_PAGE_SIZE = 5_000
private const val MAX_TARGET_INVENTORY_PAGE_SIZE = 5_000
private const val MAX_TARGET_INVENTORY_ENTRIES = 250_000
private val SAFE_PLAN_ID = Regex("[A-Za-z0-9_-]{16,128}")
private val SAFE_AUTHENTICATION_STRING = Regex("(?:[0-9]{4}-){3}[0-9]{4}")
private val SAFE_NETWORK_PAIRING_ID = Regex("[A-Za-z0-9_-]{1,128}")
private val SAFE_LOWERCASE_DIGEST = Regex("[0-9a-f]{64}")

private data class TargetInventoryUpload(
    val inventoryId: String,
    val jobId: String,
    val nextOffset: Long,
)

private fun JSONObject.toTargetInventoryUpload(
    expectedJobId: String,
    expectedInventoryId: String? = null,
): TargetInventoryUpload = TargetInventoryUpload(
    inventoryId = getString("inventoryId").also {
        check(it.matches(SAFE_LOWERCASE_DIGEST) && (expectedInventoryId == null || it == expectedInventoryId)) {
            "The node returned a different target inventory ID."
        }
    },
    jobId = getString("jobId").also {
        check(it == expectedJobId) { "The node returned a different target inventory job." }
    },
    nextOffset = getLong("nextOffset").also {
        check(it >= 0) { "The node returned an invalid target inventory offset." }
    },
)

private fun JSONObject.toTargetInventoryReference(
    expectedJobId: String,
    expectedInventoryId: String,
    draft: TargetInventoryDraft,
): TargetInventoryReference = TargetInventoryReference(
    inventoryId = getString("inventoryId"),
    jobId = getString("jobId"),
    schemaVersion = getInt("schemaVersion"),
    rootIdentity = getString("rootIdentity"),
    entryCount = getLong("entryCount"),
    totalBytes = getLong("totalBytes"),
    inventoryDigest = getString("inventoryDigest"),
).also { reference ->
    check(reference.inventoryId == expectedInventoryId && reference.inventoryId.matches(SAFE_LOWERCASE_DIGEST))
    check(reference.jobId == expectedJobId && reference.schemaVersion == 1)
    check(reference.rootIdentity == draft.rootIdentity)
    check(reference.entryCount == draft.entries.size.toLong() && reference.totalBytes == draft.totalBytes)
    check(reference.inventoryDigest.matches(SAFE_LOWERCASE_DIGEST)) {
        "The node returned an invalid target inventory digest."
    }
}

private fun TargetInventoryEntry.toJson(): JSONObject = JSONObject()
    .put("path", path)
    .put("kind", kind)
    .put("length", length)
    .put("identityToken", identityToken)
    .also { value -> modifiedAtUnixMs?.let { value.put("modifiedAtUnixMs", it) } }

internal fun targetInventoryPageDigest(entries: List<TargetInventoryEntry>): String {
    require(entries.isNotEmpty() && entries.size <= MAX_TARGET_INVENTORY_ENTRIES)
    val digest = MessageDigest.getInstance("SHA-256")
    digest.update("covalent/target-inventory-page/v1".encodeToByteArray())
    digest.update(ByteBuffer.allocate(Long.SIZE_BYTES).putLong(entries.size.toLong()).array())
    entries.forEach { entry ->
        val path = entry.path.encodeToByteArray()
        digest.update(ByteBuffer.allocate(Long.SIZE_BYTES).putLong(path.size.toLong()).array())
        digest.update(path)
        digest.update(byteArrayOf(if (entry.kind == "file") 1 else 2))
        digest.update(ByteBuffer.allocate(Long.SIZE_BYTES).putLong(entry.length).array())
        entry.modifiedAtUnixMs?.let { modified ->
            digest.update(byteArrayOf(1))
            digest.update(ByteBuffer.allocate(Long.SIZE_BYTES).putLong(modified).array())
        } ?: digest.update(byteArrayOf(0))
        val identity = entry.identityToken.encodeToByteArray()
        digest.update(ByteBuffer.allocate(Long.SIZE_BYTES).putLong(identity.size.toLong()).array())
        digest.update(identity)
    }
    return digest.digest().joinToString("") { "%02x".format(it.toInt() and 0xff) }
}

internal fun compareUtf8(left: String, right: String): Int {
    val leftBytes = left.encodeToByteArray()
    val rightBytes = right.encodeToByteArray()
    val shared = minOf(leftBytes.size, rightBytes.size)
    for (index in 0 until shared) {
        val comparison = (leftBytes[index].toInt() and 0xff).compareTo(rightBytes[index].toInt() and 0xff)
        if (comparison != 0) return comparison
    }
    return leftBytes.size.compareTo(rightBytes.size)
}

private fun validateTargetInventoryDraft(draft: TargetInventoryDraft) {
    require(draft.rootIdentity.isNotBlank() && draft.rootIdentity.length <= 512 &&
        draft.rootIdentity.none(Char::isISOControl)) { "The restore target identity is invalid." }
    require(draft.entries.size <= MAX_TARGET_INVENTORY_ENTRIES) {
        "The restore target contains too many entries."
    }
    require(draft.totalBytes >= 0) { "The restore target byte count is invalid." }
    var prior: String? = null
    var totalBytes = 0L
    draft.entries.forEach { entry ->
        val directory = when (entry.kind) {
            "file" -> false
            "directory" -> true
            else -> throw IllegalArgumentException("The restore target contains an unsupported entry.")
        }
        SafArchivePath.parse(entry.path, directory)
        require(prior == null || compareUtf8(checkNotNull(prior), entry.path) < 0) {
            "Restore target entries must be sorted and unique."
        }
        require(entry.length >= 0 && (!directory || entry.length == 0L)) {
            "The restore target contains an invalid entry length."
        }
        require(entry.modifiedAtUnixMs == null || entry.modifiedAtUnixMs >= 0)
        require(entry.identityToken.isNotBlank() && entry.identityToken.length <= 512 &&
            entry.identityToken.none(Char::isISOControl)) { "The restore target contains an invalid identity token." }
        if (!directory) totalBytes = Math.addExact(totalBytes, entry.length)
        prior = entry.path
    }
    require(totalBytes == draft.totalBytes) { "The restore target byte count changed while inventorying." }
}

private fun requireNetworkPairingId(value: String) {
    require(value.matches(SAFE_NETWORK_PAIRING_ID)) { "The network pairing ID is invalid." }
}

private fun requireNetworkPairingAddress(value: String) {
    require(value.isNotBlank() && value.length <= 253 && value.none(Char::isWhitespace)) {
        "The discovered network pairing address is invalid."
    }
}

private fun JSONObject.toNetworkPairing(): NetworkPairing {
    val pairingId = getString("pairingId")
    requireNetworkPairingId(pairingId)
    val authenticationString = getString("authenticationString")
    check(authenticationString.matches(SAFE_AUTHENTICATION_STRING)) {
        "The node returned an invalid network pairing confirmation code."
    }
    val expiresAt = getLong("expiresAtUnixMs")
    check(expiresAt > 0) { "The node returned an invalid network pairing expiry." }
    val state = when (getString("state")) {
        "awaiting_local_confirmation" -> NetworkPairingState.AWAITING_LOCAL_CONFIRMATION
        "awaiting_peer_confirmation" -> NetworkPairingState.AWAITING_PEER_CONFIRMATION
        "complete" -> NetworkPairingState.COMPLETE
        "failed" -> NetworkPairingState.FAILED
        else -> error("The node returned an unknown network pairing state.")
    }
    val peerTransport = optionalJSONObject("peerTransport")?.let { transport ->
        PeerTransport(
            peerId = transport.getString("peerId").also(::requireNetworkPairingId),
            displayName = transport.getString("displayName").also { check(it.isNotBlank() && it.length <= 128) },
            address = transport.getString("address").also(::requireNetworkPairingAddress),
            certificateDer = transport.getString("certificateDer").also { check(it.isNotBlank() && it.length <= 512 * 1024) },
            certificateFingerprint = transport.getString("certificateFingerprint")
                .also { check(it.matches(Regex("[0-9a-f]{64}"))) },
        )
    }
    check(state != NetworkPairingState.COMPLETE || peerTransport != null) {
        "A completed network pairing omitted its authenticated peer transport."
    }
    return NetworkPairing(
        pairingId = pairingId,
        direction = when (getString("direction")) {
            "incoming" -> NetworkPairingDirection.INCOMING
            "outgoing" -> NetworkPairingDirection.OUTGOING
            else -> error("The node returned an unknown network pairing direction.")
        },
        peerName = getString("peerName").also { check(it.isNotBlank() && it.length <= 128) },
        authenticationString = authenticationString,
        expiresAtUnixMs = expiresAt,
        state = state,
        failureCode = optionalString("failureCode"),
        failureMessage = optionalString("failureMessage"),
        peerTransport = peerTransport,
    )
}

private class NodeResponse(private val raw: String) {
    val body: JSONObject get() = if (raw.isBlank()) JSONObject() else JSONObject(raw)
    val array: JSONArray get() = JSONArray(raw)
    val nullableBody: JSONObject?
        get() = if (raw.isBlank() || raw.trim() == "null") null else JSONObject(raw)
}

internal fun restoreTransferPayload(reference: RestorePlanReference): JSONObject {
    check(reference.totalEntries in 0..MAX_TARGET_INVENTORY_ENTRIES.toLong()) {
        "The restore plan exceeds Android's streamed entry limit."
    }
    val request = reference.legacyPlanJson?.let { JSONObject().put("plan", JSONObject(it)) }
        ?: JSONObject().put("planId", checkNotNull(reference.planId))
    return JSONObject()
        .put("restoreRequest", request)
        .put("expectedTotalEntries", reference.totalEntries)
        .put("expectedPlanId", reference.planId)
        .put("expectedPlanDigest", reference.planDigest)
        .put("planReference", reference.toJson())
}

internal fun RestorePlanPage.toPersistenceJson(): JSONObject = JSONObject()
    .put("reference", reference.toJson())
    .put("entryOffset", entryOffset)
    .put("entries", JSONArray().apply {
        entries.forEach { entry ->
            put(JSONObject()
                .put("sourcePath", entry.sourcePath)
                .put("destinationPath", entry.destinationPath)
                .put("kind", entry.kind)
                .put("action", entry.action))
        }
    })
    .put("nextCursor", nextCursor)

internal fun restorePlanPageFromPersistence(json: JSONObject): RestorePlanPage = RestorePlanPage(
    reference = json.getJSONObject("reference").toRestorePlanReference(),
    entryOffset = json.getLong("entryOffset"),
    entries = json.getJSONArray("entries").toRestorePreviewEntries(),
    nextCursor = json.optionalString("nextCursor"),
)

internal fun restorePlanReferenceFromPersistence(json: JSONObject): RestorePlanReference =
    json.toRestorePlanReference()

private fun JSONObject.toRestorePlanReference(): RestorePlanReference {
    val inlineEntries = optJSONArray("entries")
    val durablePlanId = optionalString("planId")
    val persistedLegacyPlan = optionalString("legacyPlanJson")
    val entryCount = when {
        has("totalEntries") -> getLong("totalEntries")
        inlineEntries != null -> inlineEntries.length().toLong()
        else -> error("The node omitted the restore entry count.")
    }
    require(entryCount in 0..MAX_TARGET_INVENTORY_ENTRIES.toLong()) {
        "The restore plan exceeds Android's streamed entry limit."
    }
    return RestorePlanReference(
        planId = durablePlanId,
        planDigest = getString("planDigest"),
        backupId = getString("backupId"),
        snapshotId = getString("snapshotId"),
        authorizedRoot = getString("authorizedRoot"),
        manifestDigest = getString("manifestDigest"),
        conflictPolicy = getString("conflictPolicy"),
        jobId = getString("jobId"),
        signerDeviceId = getString("signerDeviceId"),
        signature = getString("signature"),
        totalEntries = entryCount,
        targetInventory = optionalJSONObject("targetInventory")?.toTargetInventoryBinding(),
        legacyPlanJson = persistedLegacyPlan
            ?: if (durablePlanId == null && inlineEntries != null) toString() else null,
    ).also {
        check(it.conflictPolicy in setOf("fail", "skip", "rename")) {
            "Android supports Fail, Skip, or Rename restore conflicts."
        }
        check(it.targetInventory != null || it.conflictPolicy == "fail") {
            "Nonempty restore policies require a signed target inventory."
        }
    }
}

private fun JSONObject.toTargetInventoryBinding(): TargetInventoryBinding = TargetInventoryBinding(
    schemaVersion = getInt("schemaVersion"),
    rootIdentity = getString("rootIdentity"),
    entryCount = getLong("entryCount"),
    totalBytes = getLong("totalBytes"),
    inventoryDigest = getString("inventoryDigest"),
    actionsDigest = getString("actionsDigest"),
).also { binding ->
    check(binding.schemaVersion == 1)
    check(binding.rootIdentity.isNotBlank() && binding.rootIdentity.length <= 512 &&
        binding.rootIdentity.none(Char::isISOControl))
    check(binding.entryCount in 0..MAX_TARGET_INVENTORY_ENTRIES.toLong() && binding.totalBytes >= 0)
    check(binding.inventoryDigest.matches(SAFE_LOWERCASE_DIGEST) &&
        binding.actionsDigest.matches(SAFE_LOWERCASE_DIGEST)) {
        "The signed restore inventory binding is invalid."
    }
}

private fun TargetInventoryBinding.toJson(): JSONObject = JSONObject()
    .put("schemaVersion", schemaVersion)
    .put("rootIdentity", rootIdentity)
    .put("entryCount", entryCount)
    .put("totalBytes", totalBytes)
    .put("inventoryDigest", inventoryDigest)
    .put("actionsDigest", actionsDigest)

private fun RestorePlanReference.toJson(): JSONObject = JSONObject()
    .put("planId", planId)
    .put("planDigest", planDigest)
    .put("backupId", backupId)
    .put("snapshotId", snapshotId)
    .put("authorizedRoot", authorizedRoot)
    .put("manifestDigest", manifestDigest)
    .put("conflictPolicy", conflictPolicy)
    .put("jobId", jobId)
    .put("signerDeviceId", signerDeviceId)
    .put("signature", signature)
    .put("totalEntries", totalEntries)
    .also { value -> targetInventory?.let { value.put("targetInventory", it.toJson()) } }
    .put("legacyPlanJson", legacyPlanJson)

private fun JSONArray.toRestorePreviewEntries(): List<RestorePreviewEntry> = List(length()) { index ->
    getJSONObject(index).let { entry ->
        RestorePreviewEntry(
            sourcePath = entry.optionalString("sourcePath") ?: entry.getString("destinationPath"),
            destinationPath = entry.getString("destinationPath"),
            kind = entry.getString("kind"),
            action = entry.getString("action"),
        ).also {
            check(it.kind == "directory" || it.kind == "file") {
                "The restore preview contains an unknown entry kind."
            }
            check(isSafeSafRestoreAction(it.kind, it.action)) {
                "The restore preview contains an action Android cannot apply safely."
            }
            SafArchivePath.parse(it.sourcePath, it.kind == "directory")
            SafArchivePath.parse(it.destinationPath, it.kind == "directory")
        }
    }
}

internal fun isSafeSafRestoreAction(kind: String, action: String): Boolean = when (kind) {
    "directory" -> action == "create_directory" || action == "keep_directory"
    "file" -> action == "create_file" || action == "skip_file" || action == "rename_file"
    else -> false
}

internal fun sameSignedRestorePlan(left: RestorePlanReference, right: RestorePlanReference): Boolean =
    left.planId == right.planId &&
        left.planDigest == right.planDigest &&
        left.backupId == right.backupId &&
        left.snapshotId == right.snapshotId &&
        left.authorizedRoot == right.authorizedRoot &&
        left.manifestDigest == right.manifestDigest &&
        left.conflictPolicy == right.conflictPolicy &&
        left.jobId == right.jobId &&
        left.signerDeviceId == right.signerDeviceId &&
        left.signature == right.signature &&
        left.totalEntries == right.totalEntries &&
        left.targetInventory == right.targetInventory

private fun JSONObject.toRestorePlanPage(reference: RestorePlanReference): RestorePlanPage {
    val offset = optLong("entryOffset", 0)
    val entries = getJSONArray("entries").toRestorePreviewEntries()
    check(entries.size <= MAX_RESTORE_PREVIEW_PAGE_SIZE) {
        "The restore preview page exceeds its signed page limit."
    }
    check(entries.map(RestorePreviewEntry::destinationPath).toSet().size == entries.size) {
        "The restore preview page contains a duplicate path."
    }
    if (reference.targetInventory == null) {
        check(reference.conflictPolicy == "fail" && entries.all {
            it.action == if (it.kind == "directory") "create_directory" else "create_file"
        }) { "A legacy streamed restore may only create content in an empty folder." }
    }
    val pageEnd = try {
        Math.addExact(offset, entries.size.toLong())
    } catch (_: ArithmeticException) {
        error("The restore preview page offset overflows its signed plan.")
    }
    check(offset >= 0 && pageEnd <= reference.totalEntries) {
        "The restore preview page is outside its signed plan."
    }
    val cursor = optionalString("nextCursor")
    cursor?.let { check(it.all(Char::isDigit)) { "The restore plan cursor is invalid." } }
    check(
        (pageEnd == reference.totalEntries && cursor == null) ||
            (pageEnd < reference.totalEntries && cursor == pageEnd.toString()),
    ) { "The restore preview page cursor is inconsistent." }
    return RestorePlanPage(reference, offset, entries, cursor)
}

private fun legacyRestorePlanPage(
    reference: RestorePlanReference,
    cursor: String?,
    limit: Int,
): RestorePlanPage {
    val plan = JSONObject(checkNotNull(reference.legacyPlanJson))
    val allEntries = plan.getJSONArray("entries")
    val start = cursor?.also {
        require(it.isNotEmpty() && it.all(Char::isDigit)) { "The restore plan cursor is invalid." }
    }?.toInt() ?: 0
    require(start in 0..allEntries.length()) { "The restore plan cursor is outside the plan." }
    val end = minOf(start + limit, allEntries.length())
    val pageEntries = JSONArray().apply {
        for (index in start until end) put(allEntries.getJSONObject(index))
    }.toRestorePreviewEntries()
    return RestorePlanPage(
        reference = reference,
        entryOffset = start.toLong(),
        entries = pageEntries,
        nextCursor = if (end < allEntries.length()) end.toString() else null,
    )
}

class NodeApiException(
    val statusCode: Int,
    val protocolVersion: Int,
    val code: String,
    val retryable: Boolean,
    override val message: String,
    val uploadOffset: Long? = null,
    val inventoryOffset: Long? = null,
) : Exception(message)

class NodeProtocolException(version: Int) : IllegalStateException(
    "Node uses unsupported protocol version $version.",
)

private fun URL.isLoopback(): Boolean = host.lowercase() in setOf("localhost", "127.0.0.1", "::1", "[::1]")

internal fun encodeEnrolledCertificate(input: ByteArray): String {
    require(input.size in 1..(256 * 1_024)) { "The CA certificate file is empty or too large." }
    val certificate = CertificateFactory.getInstance("X.509")
        .generateCertificate(input.inputStream()) as? X509Certificate
        ?: throw CertificateException("The selected file is not an X.509 certificate.")
    certificate.checkValidity()
    return JBase64.getEncoder().encodeToString(certificate.encoded)
}

internal fun normalizeSha256Pin(raw: String): String {
    val value = raw.trim()
    val hex = if (value.startsWith("sha256/", ignoreCase = true)) {
        val decoded = runCatching { JBase64.getDecoder().decode(value.substringAfter('/')) }
            .getOrElse { throw IllegalArgumentException("Use a valid sha256/base64 certificate pin.") }
        require(decoded.size == 32) { "A SHA-256 certificate pin must contain exactly 32 bytes." }
        decoded.joinToString("") { "%02x".format(it) }
    } else {
        value.replace(":", "").lowercase()
    }
    require(hex.length == 64 && hex.all { it in '0'..'9' || it in 'a'..'f' }) {
        "Use a 64-character hexadecimal or sha256/base64 certificate pin."
    }
    return hex
}

private fun decodeCertificate(base64: String): X509Certificate =
    CertificateFactory.getInstance("X.509")
        .generateCertificate(JBase64.getDecoder().decode(base64).inputStream()) as X509Certificate

private fun trustManagerForCertificate(certificate: X509Certificate): X509TrustManager {
    certificate.checkValidity()
    val store = KeyStore.getInstance(KeyStore.getDefaultType()).apply {
        load(null)
        setCertificateEntry("covalent-enrolled-ca", certificate)
    }
    val factory = TrustManagerFactory.getInstance(TrustManagerFactory.getDefaultAlgorithm()).apply {
        init(store)
    }
    return factory.trustManagers.filterIsInstance<X509TrustManager>().single()
}

// Dynamic certificate pinning cannot be represented by static network-security XML. The exact
// matching certificate becomes the sole trust anchor for a fresh platform trust manager; the
// platform validates the chain and HttpsURLConnection validates the hostname. A JDK interface
// proxy keeps all validation delegated to platform X.509 managers instead of implementing it.
private fun pinnedTrustManager(expectedPin: String): X509TrustManager {
    val system = systemTrustManager()
    return Proxy.newProxyInstance(
        CovalentNodeClient::class.java.classLoader,
        arrayOf(X509TrustManager::class.java),
    ) { proxy, method, arguments ->
        when (method.name) {
            "checkServerTrusted" -> {
                val certificates = certificateArguments(arguments)
                val authType = arguments?.getOrNull(1) as? String
                if (authType.isNullOrBlank()) {
                    throw CertificateException("The HTTPS authentication type is missing.")
                }
                certificates.forEach(X509Certificate::checkValidity)
                val anchor = certificates.firstOrNull { certificate ->
                    MessageDigest.getInstance("SHA-256").digest(certificate.encoded)
                        .joinToString("") { "%02x".format(it) } == expectedPin
                } ?: throw CertificateException(
                    "The HTTPS certificate does not match the enrolled SHA-256 pin.",
                )
                trustManagerForCertificate(anchor).checkServerTrusted(certificates, authType)
                null
            }
            "checkClientTrusted" -> {
                val authType = arguments?.getOrNull(1) as? String
                system.checkClientTrusted(certificateArguments(arguments), authType)
                null
            }
            "getAcceptedIssuers" -> system.acceptedIssuers
            "equals" -> proxy === arguments?.getOrNull(0)
            "hashCode" -> System.identityHashCode(proxy)
            "toString" -> "Covalent exact-pin platform trust manager"
            else -> throw UnsupportedOperationException("Unsupported trust-manager method.")
        }
    } as X509TrustManager
}

private fun certificateArguments(arguments: Array<out Any?>?): Array<X509Certificate> {
    val values = arguments?.getOrNull(0) as? Array<*>
        ?: throw CertificateException("The HTTPS server did not provide a certificate chain.")
    if (values.isEmpty()) throw CertificateException("The HTTPS server did not provide a certificate chain.")
    return Array(values.size) { index ->
        values[index] as? X509Certificate
            ?: throw CertificateException("The HTTPS server provided an invalid certificate chain.")
    }
}

private fun systemTrustManager(): X509TrustManager =
    TrustManagerFactory.getInstance(TrustManagerFactory.getDefaultAlgorithm()).apply {
        init(null as KeyStore?)
    }.trustManagers.filterIsInstance<X509TrustManager>().single()

private fun JSONObject.optionalString(key: String): String? =
    if (has(key) && !isNull(key)) getString(key) else null

private fun JSONObject.optionalJSONObject(key: String): JSONObject? =
    if (has(key) && !isNull(key)) getJSONObject(key) else null

private fun JSONObject.optionalLong(key: String): Long? =
    if (has(key) && !isNull(key)) getLong(key) else null

/** Stores the local daemon token and queued request payloads behind an Android Keystore AES key. */
class SecureNodeStore(context: Context) {
    private val preferences = context.getSharedPreferences("covalent_node", Context.MODE_PRIVATE)
    private val vault = TokenVault()

    var baseUrl: String
        get() = preferences.getString("base_url", "") ?: ""
        set(value) { preferences.edit { putString("base_url", value.trim()) } }
    var displayName: String
        get() = preferences.getString("display_name", "") ?: ""
        set(value) { preferences.edit { putString("display_name", value.trim()) } }
    var token: String
        get() = preferences.getString("token", null)?.let(vault::decrypt).orEmpty()
        set(value) { preferences.edit {
            if (value.isBlank()) remove("token") else putString("token", vault.encrypt(value))
        } }

    fun enrolledTrust(): EnrolledTrust? {
        val certificate = preferences.getString(TRUST_CA_KEY, null)?.let(vault::decrypt)
        val pin = preferences.getString(TRUST_PIN_KEY, null)?.let(vault::decrypt)
        return if (certificate.isNullOrBlank() && pin.isNullOrBlank()) null else EnrolledTrust(certificate, pin)
    }

    fun saveEnrolledTrust(trust: EnrolledTrust?) {
        preferences.edit {
            if (trust?.caCertificateDerBase64.isNullOrBlank()) remove(TRUST_CA_KEY)
            else putString(TRUST_CA_KEY, vault.encrypt(checkNotNull(trust?.caCertificateDerBase64)))
            if (trust?.sha256Pin.isNullOrBlank()) remove(TRUST_PIN_KEY)
            else putString(TRUST_PIN_KEY, vault.encrypt(normalizeSha256Pin(checkNotNull(trust?.sha256Pin))))
        }
    }

    fun savePending(
        jobId: String,
        path: String,
        payload: JSONObject,
        mode: String = "json",
        treeUri: String? = null,
    ) {
        preferences.edit { putString("pending_$jobId", vault.encrypt(JSONObject().apply {
            put("path", path)
            put("payload", payload)
            put("mode", mode)
            treeUri?.let { put("treeUri", it) }
        }.toString())) }
    }

    fun pending(jobId: String): JSONObject? = preferences.getString("pending_$jobId", null)
        ?.let(vault::decrypt)?.let(::JSONObject)

    fun pendingJobIds(): List<String> = preferences.all.keys.asSequence()
        .filter { it.startsWith(PENDING_PREFIX) }
        .map { it.removePrefix(PENDING_PREFIX) }
        .filter { it.isNotBlank() }
        .sorted()
        .toList()

    fun runnablePendingJobIds(): List<String> = pendingJobIds().filter { jobId ->
        transfer(jobId)?.state?.let { it == TransferState.QUEUED || it == TransferState.RUNNING } ?: true
    }

    fun removePending(jobId: String) = preferences.edit { remove("pending_$jobId") }

    fun savePendingAcknowledgement(jobId: String, completionDetail: String) {
        preferences.edit {
            putString(
                "$ACKNOWLEDGEMENT_PREFIX$jobId",
                vault.encrypt(JSONObject().put("completionDetail", completionDetail).toString()),
            )
        }
    }

    fun pendingAcknowledgementJobIds(): List<String> = preferences.all.keys.asSequence()
        .filter { it.startsWith(ACKNOWLEDGEMENT_PREFIX) }
        .map { it.removePrefix(ACKNOWLEDGEMENT_PREFIX) }
        .filter(String::isNotBlank)
        .sorted()
        .toList()

    fun acknowledgementCompletionDetail(jobId: String): String? = preferences
        .getString("$ACKNOWLEDGEMENT_PREFIX$jobId", null)
        ?.let(vault::decrypt)
        ?.let(::JSONObject)
        ?.optString("completionDetail")

    fun removePendingAcknowledgement(jobId: String) = preferences.edit {
        remove("$ACKNOWLEDGEMENT_PREFIX$jobId")
    }

    fun savePendingDiscard(jobId: String, completionDetail: String) {
        preferences.edit {
            putString(
                "$DISCARD_PREFIX$jobId",
                vault.encrypt(JSONObject().put("completionDetail", completionDetail).toString()),
            )
        }
    }

    fun pendingDiscardJobIds(): List<String> = preferences.all.keys.asSequence()
        .filter { it.startsWith(DISCARD_PREFIX) }
        .map { it.removePrefix(DISCARD_PREFIX) }
        .filter(String::isNotBlank)
        .sorted()
        .toList()

    fun discardCompletionDetail(jobId: String): String? = preferences
        .getString("$DISCARD_PREFIX$jobId", null)
        ?.let(vault::decrypt)
        ?.let(::JSONObject)
        ?.optString("completionDetail")

    fun removePendingDiscard(jobId: String) = preferences.edit {
        remove("$DISCARD_PREFIX$jobId")
    }

    fun saveTransfer(record: TransferRecord) {
        val value = JSONObject()
            .put("jobId", record.jobId)
            .put("label", record.label)
            .put("kind", record.kind.name.lowercase())
            .put("state", record.state.name.lowercase())
            .put("detail", record.detail)
            .put("completedBytes", record.completedBytes)
            .put("totalBytes", record.totalBytes)
            .put("completedEntries", record.completedEntries)
            .put("totalEntries", record.totalEntries)
            .put("updatedAtUnixMs", record.updatedAtUnixMs)
            .put("retryable", record.retryable)
        preferences.edit { putString("$TRANSFER_PREFIX${record.jobId}", vault.encrypt(value.toString())) }
    }

    fun transfer(jobId: String): TransferRecord? = preferences
        .getString("$TRANSFER_PREFIX$jobId", null)
        ?.let(vault::decrypt)
        ?.let(::JSONObject)
        ?.toTransferRecord()

    fun transfers(): List<TransferRecord> = preferences.all.keys.asSequence()
        .filter { it.startsWith(TRANSFER_PREFIX) }
        .mapNotNull { transfer(it.removePrefix(TRANSFER_PREFIX)) }
        .sortedByDescending(TransferRecord::updatedAtUnixMs)
        .toList()

    @Synchronized
    fun updateTransfer(jobId: String, update: (TransferRecord) -> TransferRecord): TransferRecord? {
        val current = transfer(jobId) ?: return null
        return update(current).copy(updatedAtUnixMs = System.currentTimeMillis()).also(::saveTransfer)
    }

    fun saveWorkflow(name: String, value: JSONObject?) {
        preferences.edit {
            if (value == null) remove("$WORKFLOW_PREFIX$name")
            else putString("$WORKFLOW_PREFIX$name", vault.encrypt(value.toString()))
        }
    }

    fun workflow(name: String): JSONObject? = preferences.getString("$WORKFLOW_PREFIX$name", null)
        ?.let(vault::decrypt)
        ?.let(::JSONObject)

    fun rememberedBackups(): List<RememberedBackup> {
        val data = preferences.getString("backups", "[]") ?: "[]"
        val values = JSONArray(data)
        return List(values.length()) { index ->
            values.getJSONObject(index).let {
                RememberedBackup(
                    backupId = it.getString("backupId"),
                    name = it.getString("name"),
                    ownerDeviceId = it.optString("ownerDeviceId"),
                    latestSnapshotId = it.optionalString("latestSnapshotId")
                        ?: it.optionalString("snapshotId"),
                    latestCommittedAtUnixMs = it.optionalLong("latestCommittedAtUnixMs")
                        ?: it.optionalLong("createdAtMillis"),
                    snapshotCount = it.optLong("snapshotCount", if (it.has("snapshotId")) 1 else 0),
                    selectedProviderIds = it.optJSONArray("selectedProviderIds")?.let { providers ->
                        buildSet { repeat(providers.length()) { add(providers.getString(it)) } }
                    }.orEmpty(),
                )
            }
        }.sortedByDescending { it.latestCommittedAtUnixMs ?: 0L }
    }

    fun replaceBackups(backups: List<RememberedBackup>) {
        val values = JSONArray().apply {
            backups.forEach { backup ->
                put(JSONObject()
                    .put("backupId", backup.backupId)
                    .put("name", backup.name)
                    .put("ownerDeviceId", backup.ownerDeviceId)
                    .put("latestSnapshotId", backup.latestSnapshotId)
                    .put("latestCommittedAtUnixMs", backup.latestCommittedAtUnixMs)
                    .put("snapshotCount", backup.snapshotCount)
                    .put("selectedProviderIds", JSONArray(backup.selectedProviderIds.toList().sorted())))
            }
        }
        preferences.edit { putString("backups", values.toString()) }
    }

    private class TokenVault {
        private val alias = "covalent.local.token.v1"
        private fun key(): SecretKey {
            val store = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
            (store.getKey(alias, null) as? SecretKey)?.let { return it }
            return KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore").apply {
                init(KeyGenParameterSpec.Builder(alias, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
                    .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                    .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                    .build())
            }.generateKey()
        }
        fun encrypt(value: String): String {
            val cipher = Cipher.getInstance("AES/GCM/NoPadding").apply { init(Cipher.ENCRYPT_MODE, key()) }
            return Base64.encodeToString(cipher.iv, Base64.NO_WRAP) + ":" +
                Base64.encodeToString(cipher.doFinal(value.encodeToByteArray()), Base64.NO_WRAP)
        }
        fun decrypt(value: String): String {
            val parts = value.split(":", limit = 2)
            require(parts.size == 2) { "Saved local token is invalid; enter it again." }
            val cipher = Cipher.getInstance("AES/GCM/NoPadding").apply {
                init(Cipher.DECRYPT_MODE, key(), GCMParameterSpec(128, Base64.decode(parts[0], Base64.NO_WRAP)))
            }
            return cipher.doFinal(Base64.decode(parts[1], Base64.NO_WRAP)).decodeToString()
        }
    }

    private companion object {
        const val PENDING_PREFIX = "pending_"
        const val ACKNOWLEDGEMENT_PREFIX = "acknowledgement_"
        const val DISCARD_PREFIX = "discard_"
        const val TRANSFER_PREFIX = "transfer_"
        const val WORKFLOW_PREFIX = "workflow_"
        const val TRUST_CA_KEY = "tls_ca_der"
        const val TRUST_PIN_KEY = "tls_sha256_pin"
    }
}

private fun JSONObject.toTransferRecord(): TransferRecord = TransferRecord(
    jobId = getString("jobId"),
    label = getString("label"),
    kind = TransferKind.valueOf(getString("kind").uppercase()),
    state = TransferState.valueOf(getString("state").uppercase()),
    detail = optString("detail"),
    completedBytes = optLong("completedBytes", 0),
    totalBytes = optionalLong("totalBytes"),
    completedEntries = optLong("completedEntries", 0),
    totalEntries = optionalLong("totalEntries"),
    updatedAtUnixMs = optLong("updatedAtUnixMs", 0),
    retryable = optBoolean("retryable", false),
)

fun newId(prefix: String): String = "$prefix-${UUID.randomUUID().toString().replace("-", "").take(20)}"
