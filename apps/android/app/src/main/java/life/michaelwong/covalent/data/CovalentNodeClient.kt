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
import life.michaelwong.covalent.model.PeerGrant
import life.michaelwong.covalent.model.Provider
import life.michaelwong.covalent.model.ProviderReachability
import life.michaelwong.covalent.model.RememberedBackup
import life.michaelwong.covalent.model.RestorePlanPage
import life.michaelwong.covalent.model.RestorePlanReference
import life.michaelwong.covalent.model.RestorePreviewEntry
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
        peerId: String,
        address: String,
        certificateDer: String,
    ): Provider {
        val json = post(
            baseUrl,
            token,
            "/api/v1/providers/connect",
            JSONObject()
                .put("peerId", peerId)
                .put("address", address)
                .put("certificateDer", certificateDer),
        )
        return Provider(
            peerId = json.getString("peerId"),
            address = json.getString("address"),
            fingerprint = json.getString("certificateFingerprint"),
            reachability = ProviderReachability.CONNECTED,
        )
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
        check(pageReference.planId == reference.planId) { "The node returned a different restore plan ID." }
        check(pageReference.planDigest == reference.planDigest) {
            "The node returned a different signed restore plan."
        }
        check(pageReference.jobId == reference.jobId) { "The node returned a different restore job." }
        check(pageReference.totalEntries == reference.totalEntries) {
            "The restore plan changed while previewing it."
        }
        return page.toRestorePlanPage(pageReference).also {
            check(it.entryOffset == (parsedCursor?.toLong() ?: 0L)) {
                "The node returned a different restore preview page."
            }
        }
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

private const val COVALENT_PROTOCOL_VERSION = 1
private const val RESTORE_PREVIEW_PAGE_SIZE = 100
private const val MAX_RESTORE_PREVIEW_PAGE_SIZE = 1_000
private val SAFE_PLAN_ID = Regex("[A-Za-z0-9_-]{16,128}")

private class NodeResponse(private val raw: String) {
    val body: JSONObject get() = if (raw.isBlank()) JSONObject() else JSONObject(raw)
    val array: JSONArray get() = JSONArray(raw)
    val nullableBody: JSONObject?
        get() = if (raw.isBlank() || raw.trim() == "null") null else JSONObject(raw)
}

internal fun restoreTransferPayload(reference: RestorePlanReference): JSONObject {
    check(reference.totalEntries in 0..100_000) {
        "The restore plan exceeds Android's streamed entry limit."
    }
    val request = reference.legacyPlanJson?.let { JSONObject().put("plan", JSONObject(it)) }
        ?: JSONObject().put("planId", checkNotNull(reference.planId))
    return JSONObject()
        .put("restoreRequest", request)
        .put("expectedTotalEntries", reference.totalEntries)
        .put("expectedPlanId", reference.planId)
        .put("expectedPlanDigest", reference.planDigest)
}

internal fun RestorePlanPage.toPersistenceJson(): JSONObject = JSONObject()
    .put("reference", reference.toJson())
    .put("entryOffset", entryOffset)
    .put("entries", JSONArray().apply {
        entries.forEach { entry ->
            put(JSONObject()
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

private fun JSONObject.toRestorePlanReference(): RestorePlanReference {
    val inlineEntries = optJSONArray("entries")
    val durablePlanId = optionalString("planId")
    val persistedLegacyPlan = optionalString("legacyPlanJson")
    val entryCount = when {
        has("totalEntries") -> getLong("totalEntries")
        inlineEntries != null -> inlineEntries.length().toLong()
        else -> error("The node omitted the restore entry count.")
    }
    require(entryCount in 0..100_000) { "The restore plan exceeds Android's streamed entry limit." }
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
        legacyPlanJson = persistedLegacyPlan
            ?: if (durablePlanId == null && inlineEntries != null) toString() else null,
    ).also {
        check(it.conflictPolicy == "fail") {
            "Streamed restores require the fail-on-conflict policy and an empty folder."
        }
    }
}

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
    .put("legacyPlanJson", legacyPlanJson)

private fun JSONArray.toRestorePreviewEntries(): List<RestorePreviewEntry> = List(length()) { index ->
    getJSONObject(index).let { entry ->
        RestorePreviewEntry(
            destinationPath = entry.getString("destinationPath"),
            kind = entry.getString("kind"),
            action = entry.getString("action"),
        ).also {
            check(it.kind == "directory" || it.kind == "file") {
                "The restore preview contains an unknown entry kind."
            }
            check(
                it.action == if (it.kind == "directory") "create_directory" else "create_file",
            ) { "The restore preview could modify existing content." }
            SafArchivePath.parse(it.destinationPath, it.kind == "directory")
        }
    }
}

private fun JSONObject.toRestorePlanPage(reference: RestorePlanReference): RestorePlanPage {
    val offset = optLong("entryOffset", 0)
    val entries = getJSONArray("entries").toRestorePreviewEntries()
    check(entries.map(RestorePreviewEntry::destinationPath).toSet().size == entries.size) {
        "The restore preview page contains a duplicate path."
    }
    check(offset >= 0 && offset + entries.size <= reference.totalEntries) {
        "The restore preview page is outside its signed plan."
    }
    val cursor = optionalString("nextCursor")
    cursor?.let { check(it.all(Char::isDigit)) { "The restore plan cursor is invalid." } }
    val pageEnd = offset + entries.size
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
