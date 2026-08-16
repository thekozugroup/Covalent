package life.michaelwong.covalent.data

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import androidx.core.content.edit
import java.net.HttpURLConnection
import java.net.URL
import java.security.KeyStore
import java.util.UUID
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec
import org.json.JSONArray
import org.json.JSONObject
import life.michaelwong.covalent.model.DiscoveryCandidate
import life.michaelwong.covalent.model.NodeStatus
import life.michaelwong.covalent.model.Provider
import life.michaelwong.covalent.model.RememberedBackup

/** A small, contract-first client for the authenticated local Covalent node API. */
class CovalentNodeClient {
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
        val values = request(baseUrl, "GET", "/api/v1/providers", token, null).array
        return List(values.length()) { index ->
            values.getJSONObject(index).let {
                Provider(it.getString("peerId"), it.getString("address"), it.getString("certificateFingerprint"))
            }
        }
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
        val url = runCatching { URL(normalized + path) }.getOrElse {
            throw IllegalArgumentException("Use a complete Covalent node URL.")
        }
        require(url.protocol == "http" || url.protocol == "https") {
            "Use a complete HTTP or HTTPS Covalent node URL."
        }
        if (!token.isNullOrBlank() && url.protocol != "https" && !url.isLoopback()) {
            throw IllegalArgumentException(
                "Covalent sends its API token only over HTTPS or device loopback. Configure HTTPS for a network node.",
            )
        }
        return (url.openConnection() as HttpURLConnection).apply {
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

private class NodeResponse(private val raw: String) {
    val body: JSONObject get() = if (raw.isBlank()) JSONObject() else JSONObject(raw)
    val array: JSONArray get() = JSONArray(raw)
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

    fun removePending(jobId: String) = preferences.edit { remove("pending_$jobId") }

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
    }
}

fun newId(prefix: String): String = "$prefix-${UUID.randomUUID().toString().replace("-", "").take(20)}"
