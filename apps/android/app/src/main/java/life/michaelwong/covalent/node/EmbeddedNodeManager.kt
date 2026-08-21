package life.michaelwong.covalent.node

import android.Manifest
import android.content.Context
import android.content.Intent
import android.content.SharedPreferences
import android.content.pm.PackageManager
import android.net.wifi.WifiManager
import android.os.Build
import android.os.storage.StorageManager
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import androidx.core.content.ContextCompat
import androidx.core.content.edit
import java.io.File
import java.security.SecureRandom
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import life.michaelwong.covalent.model.NodeConnection

/** Visible provider state for a future explicit Android-device storage toggle. */
data class EmbeddedProviderState(
    val supported: Boolean,
    val keyProtectionAvailable: Boolean,
    val enabled: Boolean,
    val running: Boolean,
    val statusMessage: String,
    val usedBytes: Long,
    val reservedBytes: Long,
    val availableBytes: Long,
    val maxBytes: Long,
    val keepFreeBytes: Long,
    val lanDiscoveryRequested: Boolean,
)

/**
 * Explicit opt-in local-node owner. It never replaces a configured external node.
 *
 * The service is only started after [enable]. Until Rust can consume a Keystore-backed
 * identity protector, start remains fail-closed and external-node mode stays available.
 */
class EmbeddedNodeManager(context: Context) {
    private val applicationContext = context.applicationContext
    private val preferences: SharedPreferences = applicationContext.getSharedPreferences(
        PREFERENCES_NAME,
        Context.MODE_PRIVATE,
    )
    private val localStore = LocalEmbeddedNodeStore(applicationContext)
    private var multicastLock: WifiManager.MulticastLock? = null

    val state: StateFlow<EmbeddedProviderState> = sharedState.asStateFlow()

    init {
        sharedState.value = readPersistedState()
    }

    fun enable(maxBytes: Long, keepFreeBytes: Long, lanDiscoveryRequested: Boolean = false) {
        if (!keyProtectionAvailable()) {
            publish(
                enabled = false,
                running = false,
                message = "This phone cannot protect its Covalent identity yet, so it cannot store backups.",
                reservedBytes = 0L,
                availableBytes = availableBytes(),
            )
            return
        }
        val capacityError = capacityValidationMessage(maxBytes, keepFreeBytes)
        if (capacityError != null) {
            publish(
                enabled = false,
                running = false,
                message = capacityError,
                reservedBytes = 0L,
                availableBytes = availableBytes(),
            )
            return
        }
        preferences.edit {
            putBoolean(KEY_ENABLED, true)
            putLong(KEY_MAX_BYTES, maxBytes)
            putLong(KEY_KEEP_FREE_BYTES, keepFreeBytes)
            putBoolean(KEY_LAN_REQUESTED, lanDiscoveryRequested)
        }
        startService()
    }

    fun disable() {
        preferences.edit {
            putBoolean(KEY_ENABLED, false)
            putString(KEY_ACTIVE_MODE, NodeMode.EXTERNAL.wireValue)
        }
        releaseMulticastLock()
        applicationContext.startService(
            Intent(applicationContext, NodeProviderService::class.java)
                .setAction(NodeProviderService.ACTION_STOP),
        )
        publish(
            enabled = false,
            running = false,
            message = "Storing backups on this phone is off. External node connections remain unchanged.",
            reservedBytes = 0L,
            availableBytes = availableBytes(),
        )
    }

    /** Reconnects only an explicitly enabled local provider after process/service recreation. */
    fun reconnectIfEnabled() {
        if (preferences.getBoolean(KEY_ENABLED, false)) startService()
    }

    /** The selected controller mode; external remains the default and fallback. */
    fun activeMode(): NodeMode = NodeMode.fromWire(
        preferences.getString(KEY_ACTIVE_MODE, NodeMode.EXTERNAL.wireValue),
    )

    /** Restores the separately persisted external connection without changing its credentials. */
    fun selectExternalMode() {
        preferences.edit { putString(KEY_ACTIVE_MODE, NodeMode.EXTERNAL.wireValue) }
        val enabled = preferences.getBoolean(KEY_ENABLED, false)
        publish(
            enabled = enabled,
            running = preferences.getBoolean(KEY_RUNNING, false),
            message = "A separate backup server is selected. Storing backups on this phone stays a separate choice.",
            reservedBytes = if (enabled) preferences.getLong(KEY_MAX_BYTES, DEFAULT_MAX_BYTES) else 0L,
            availableBytes = availableBytes(),
        )
    }

    /** Selects the independently stored local connection without modifying external credentials. */
    fun selectLocalMode(): Boolean {
        val enabled = preferences.getBoolean(KEY_ENABLED, false)
        val running = preferences.getBoolean(KEY_RUNNING, false)
        if (!enabled || !running || localStore.baseUrl.isBlank() || localStore.token.isBlank()) {
            publish(
                enabled = enabled,
                running = running,
                message = "This phone is not ready to act as your backup server.",
                reservedBytes = if (enabled) preferences.getLong(KEY_MAX_BYTES, DEFAULT_MAX_BYTES) else 0L,
                availableBytes = availableBytes(),
            )
            return false
        }
        preferences.edit { putString(KEY_ACTIVE_MODE, NodeMode.LOCAL.wireValue) }
        publish(
            enabled = true,
            running = true,
            message = "This phone is selected as your backup server.",
            reservedBytes = preferences.getLong(KEY_MAX_BYTES, DEFAULT_MAX_BYTES),
            availableBytes = availableBytes(),
        )
        return true
    }

    /** Returns local credentials only to the in-process client selector; they are never exported. */
    fun localConnectionForActiveMode(): NodeConnection? =
        if (activeMode() == NodeMode.LOCAL && localStore.baseUrl.isNotBlank() && localStore.token.isNotBlank()) {
            NodeConnection(localStore.baseUrl, localStore.token)
        } else {
            null
        }

    internal fun serviceStart(): NativeNodeResponse {
        return runCatching {
            serviceStartSafely()
        }.getOrElse {
            releaseMulticastLock()
            unavailable("Covalent could not reach this app's private storage.")
        }
    }

    private fun serviceStartSafely(): NativeNodeResponse {
        if (!preferences.getBoolean(KEY_ENABLED, false)) {
            return NativeNodeResponse(
                ok = true,
                code = "disabled",
                message = "Storing backups on this phone is off.",
                handle = null,
                apiBaseUrl = null,
                peerAddress = null,
                state = "stopped",
            )
        }
        val maxBytes = preferences.getLong(KEY_MAX_BYTES, DEFAULT_MAX_BYTES)
        val keepFreeBytes = preferences.getLong(KEY_KEEP_FREE_BYTES, DEFAULT_KEEP_FREE_BYTES)
        capacityValidationMessage(maxBytes, keepFreeBytes)?.let { return unavailable(it) }
        if (!hasPeerNetworkPermission()) {
            return unavailable("Allow local network access before this phone can start storing backups.")
        }
        val token = protectedTokenBytes()
        return try {
            val lanEnabled = acquireLanDiscoveryPermission()
            CovalentNative.start(
                dataDirectory = privateNodeDirectory().path,
                deviceName = "${Build.MODEL.take(64)} Android",
                lanDiscoveryEnabled = lanEnabled,
                apiToken = token,
                maximumTotalBytes = maxBytes,
                freeSpaceReserveBytes = keepFreeBytes,
            ).also { response ->
                if (response.ok && response.apiBaseUrl != null) {
                    localStore.baseUrl = response.apiBaseUrl
                    preferences.edit { putString(KEY_ACTIVE_MODE, NodeMode.LOCAL.wireValue) }
                } else {
                    releaseMulticastLock()
                }
            }
        } finally {
            token.fill(0)
        }
    }

    internal fun serviceStop(handle: Long): NativeNodeResponse {
        releaseMulticastLock()
        return if (handle > 0L) CovalentNative.stop(handle) else NativeNodeResponse(
            ok = true,
            code = "ok",
            message = "Storing backups on this phone is stopped.",
            handle = null,
            apiBaseUrl = null,
            peerAddress = null,
            state = "stopped",
        )
    }

    internal fun report(response: NativeNodeResponse) {
        val enabled = preferences.getBoolean(KEY_ENABLED, false)
        publish(
            enabled = enabled,
            running = response.ok && response.state == "running",
            message = response.message,
            reservedBytes = if (enabled) preferences.getLong(KEY_MAX_BYTES, DEFAULT_MAX_BYTES) else 0L,
            availableBytes = availableBytes(),
        )
    }

    private fun startService() {
        val intent = Intent(applicationContext, NodeProviderService::class.java)
            .setAction(NodeProviderService.ACTION_START)
        ContextCompat.startForegroundService(applicationContext, intent)
    }

    private fun protectedTokenBytes(): ByteArray {
        val current = localStore.token.ifBlank {
            ByteArray(32).also(SecureRandom()::nextBytes).let { generated ->
                try {
                    Base64.encodeToString(generated, Base64.NO_WRAP or Base64.URL_SAFE)
                } finally {
                    generated.fill(0)
                }
            }.also { localStore.token = it }
        }
        return current.encodeToByteArray()
    }

    private fun acquireLanDiscoveryPermission(): Boolean {
        val requested = preferences.getBoolean(KEY_LAN_REQUESTED, false)
        if (!requested || !hasLanDiscoveryPermission()) return false
        val wifiManager = applicationContext.getSystemService(WifiManager::class.java) ?: return false
        multicastLock = wifiManager.createMulticastLock("covalent-local-discovery").apply {
            setReferenceCounted(false)
            acquire()
        }
        return true
    }

    /** Permission for the wildcard QUIC peer socket; multicast is optional. */
    private fun hasPeerNetworkPermission(): Boolean =
        Build.VERSION.SDK_INT < 37 || ContextCompat.checkSelfPermission(
            applicationContext,
            Manifest.permission.ACCESS_LOCAL_NETWORK,
        ) == PackageManager.PERMISSION_GRANTED

    private fun hasLanDiscoveryPermission(): Boolean =
        ContextCompat.checkSelfPermission(
            applicationContext,
            Manifest.permission.CHANGE_WIFI_MULTICAST_STATE,
        ) == PackageManager.PERMISSION_GRANTED &&
            hasPeerNetworkPermission()

    private fun releaseMulticastLock() {
        multicastLock?.takeIf(WifiManager.MulticastLock::isHeld)?.release()
        multicastLock = null
    }

    private fun privateNodeDirectory(): File {
        val root = applicationContext.noBackupFilesDir.canonicalFile
        val directory = File(root, "covalent-node").canonicalFile
        check(directory.parentFile == root) { "Android private node directory is invalid." }
        check(directory.mkdirs() || directory.isDirectory) { "Android private node directory is unavailable." }
        return directory
    }

    /** Null only when the requested provider cap fits current private storage safely. */
    fun capacityValidationMessage(maxBytes: Long, keepFreeBytes: Long): String? {
        if (maxBytes < MIN_PROVIDER_BYTES || keepFreeBytes < 0L || keepFreeBytes > maxBytes - MIN_PROVIDER_BYTES) {
            return "Choose at least 512 MB and leave protected free space."
        }
        val used = usedBytes()
        if (used > maxBytes) {
            return "This phone already holds more than the new limit. Choose a limit at least as large as what is already stored."
        }
        val allocatableAfterReserve = availableBytes().saturatingSub(keepFreeBytes)
        if (maxBytes > used.saturatingAdd(allocatableAfterReserve)) {
            return "The new limit is larger than the space left once the free space you asked to keep is set aside."
        }
        return null
    }

    private fun availableBytes(): Long = runCatching {
        applicationContext.getSystemService(StorageManager::class.java)
            .getAllocatableBytes(StorageManager.UUID_DEFAULT)
            .coerceAtLeast(0L)
    }.getOrElse { applicationContext.noBackupFilesDir.usableSpace.coerceAtLeast(0L) }

    private fun Long.saturatingAdd(other: Long): Long =
        if (Long.MAX_VALUE - this < other) Long.MAX_VALUE else this + other

    private fun Long.saturatingSub(other: Long): Long =
        if (this <= other) 0L else this - other

    private fun unavailable(message: String) = NativeNodeResponse(
        ok = false,
        code = "embedded_provider_unavailable",
        message = message,
        handle = null,
        apiBaseUrl = null,
        peerAddress = null,
        state = "stopped",
    )

    private fun readPersistedState(): EmbeddedProviderState {
        val enabled = preferences.getBoolean(KEY_ENABLED, false)
        return EmbeddedProviderState(
            supported = secureProviderSupported(),
            keyProtectionAvailable = keyProtectionAvailable(),
            enabled = enabled,
            running = preferences.getBoolean(KEY_RUNNING, false),
            statusMessage = preferences.getString(KEY_STATUS, null)
                ?: if (enabled) "This phone will start storing backups again once secure storage is available." else "Storing backups on this phone is off.",
            usedBytes = usedBytes(),
            reservedBytes = if (enabled) preferences.getLong(KEY_MAX_BYTES, DEFAULT_MAX_BYTES) else 0L,
            availableBytes = availableBytes(),
            maxBytes = preferences.getLong(KEY_MAX_BYTES, DEFAULT_MAX_BYTES),
            keepFreeBytes = preferences.getLong(KEY_KEEP_FREE_BYTES, DEFAULT_KEEP_FREE_BYTES),
            lanDiscoveryRequested = preferences.getBoolean(KEY_LAN_REQUESTED, false),
        )
    }

    private fun publish(
        enabled: Boolean,
        running: Boolean,
        message: String,
        reservedBytes: Long,
        availableBytes: Long,
    ) {
        preferences.edit {
            putBoolean(KEY_RUNNING, running)
            putString(KEY_STATUS, message)
        }
        sharedState.value = EmbeddedProviderState(
            supported = secureProviderSupported(),
            keyProtectionAvailable = keyProtectionAvailable(),
            enabled = enabled,
            running = running,
            statusMessage = message,
            usedBytes = usedBytes(),
            reservedBytes = reservedBytes,
            availableBytes = availableBytes,
            maxBytes = preferences.getLong(KEY_MAX_BYTES, DEFAULT_MAX_BYTES),
            keepFreeBytes = preferences.getLong(KEY_KEEP_FREE_BYTES, DEFAULT_KEEP_FREE_BYTES),
            lanDiscoveryRequested = preferences.getBoolean(KEY_LAN_REQUESTED, false),
        )
    }

    private companion object {
        const val PREFERENCES_NAME = "covalent_embedded_provider"
        const val KEY_ENABLED = "enabled"
        const val KEY_MAX_BYTES = "maximum_total_bytes"
        const val KEY_KEEP_FREE_BYTES = "keep_free_bytes"
        const val KEY_LAN_REQUESTED = "lan_requested"
        const val KEY_ACTIVE_MODE = "active_mode"
        const val KEY_RUNNING = "running"
        const val KEY_STATUS = "status"
        const val MIN_PROVIDER_BYTES = 512L * 1024L * 1024L
        const val DEFAULT_MAX_BYTES = 2L * 1024L * 1024L * 1024L
        const val DEFAULT_KEEP_FREE_BYTES = 512L * 1024L * 1024L
        val sharedState = MutableStateFlow(
            EmbeddedProviderState(false, false, false, false, "Storing backups on this phone is off.", 0L, 0L, 0L, 0L, 0L, false),
        )
    }

    /** True only after Rust accepts a non-exportable Android Keystore identity protector. */
    fun keyProtectionAvailable(): Boolean = false

    private fun secureProviderSupported(): Boolean = CovalentNative.isAvailable && keyProtectionAvailable()

    private fun usedBytes(): Long = runCatching {
        privateNodeDirectory().walkTopDown().filter(File::isFile).sumOf(File::length)
    }.getOrDefault(0L)
}

enum class NodeMode(val wireValue: String) {
    EXTERNAL("external"),
    LOCAL("local");

    companion object {
        fun fromWire(value: String?): NodeMode = entries.firstOrNull { it.wireValue == value } ?: EXTERNAL
    }
}

/** Separate Android Keystore alias and encrypted storage for local-node-only credentials. */
private class LocalEmbeddedNodeStore(context: Context) {
    private val preferences = context.getSharedPreferences("covalent_embedded_node_credentials", Context.MODE_PRIVATE)

    var baseUrl: String
        get() = preferences.getString("base_url", "") ?: ""
        set(value) = preferences.edit { putString("base_url", value.trim()) }

    var token: String
        get() = preferences.getString("token", null)?.let(::decrypt).orEmpty()
        set(value) {
            if (value.isBlank()) preferences.edit { remove("token") }
            else preferences.edit { putString("token", encrypt(value)) }
        }

    private fun key(): SecretKey {
        val store = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
        (store.getKey(KEY_ALIAS, null) as? SecretKey)?.let { return it }
        return KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore").apply {
            init(
                KeyGenParameterSpec.Builder(KEY_ALIAS, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
                    .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                    .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                    .build(),
            )
        }.generateKey()
    }

    private fun encrypt(value: String): String {
        val cipher = Cipher.getInstance("AES/GCM/NoPadding").apply { init(Cipher.ENCRYPT_MODE, key()) }
        return Base64.encodeToString(cipher.iv, Base64.NO_WRAP) + ":" +
            Base64.encodeToString(cipher.doFinal(value.encodeToByteArray()), Base64.NO_WRAP)
    }

    private fun decrypt(value: String): String {
        val parts = value.split(":", limit = 2)
        require(parts.size == 2) { "Embedded node credentials are invalid." }
        val cipher = Cipher.getInstance("AES/GCM/NoPadding").apply {
            init(Cipher.DECRYPT_MODE, key(), GCMParameterSpec(128, Base64.decode(parts[0], Base64.NO_WRAP)))
        }
        return cipher.doFinal(Base64.decode(parts[1], Base64.NO_WRAP)).decodeToString()
    }

    private companion object {
        const val KEY_ALIAS = "covalent.embedded.node.token.v1"
    }
}
