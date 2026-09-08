package life.michaelwong.covalent.node

import life.michaelwong.covalent.BuildConfig
import org.json.JSONObject

/** Fixed direct JNI ABI. Native methods are registered by JNI_OnLoad, never name-mangled. */
internal object CovalentNative {
    private val libraryLoaded: Boolean = runCatching {
        System.loadLibrary("covalent_android_jni")
    }.isSuccess

    val isAvailable: Boolean get() = libraryLoaded

    @JvmStatic
    private external fun nativeStart(
        dataDirectory: String,
        deviceName: String,
        lanDiscoveryEnabled: Boolean,
        apiToken: ByteArray,
        keyEncryptionKey: ByteArray,
        keyVersion: Int,
        maximumTotalBytes: Long,
        freeSpaceReserveBytes: Long,
        keyProtectionLevel: Int,
        backupProviderEnabled: Boolean,
        syncPackageInvalid: Boolean,
        folderSyncAccessUnavailable: Boolean,
        syncGuardianPath: String,
        syncGuardianSha256: String,
        syncWorkerPath: String,
        syncWorkerSha256: String,
        syncRuntimeDirectory: String,
        syncListenerPort: Int,
    ): String

    @JvmStatic
    private external fun nativeRecoverStart(
        dataDirectory: String,
        deviceName: String,
        lanDiscoveryEnabled: Boolean,
        apiToken: ByteArray,
        keyEncryptionKey: ByteArray,
        keyVersion: Int,
        maximumTotalBytes: Long,
        freeSpaceReserveBytes: Long,
        keyProtectionLevel: Int,
        recoveryKit: ByteArray,
        recoveryKey: ByteArray,
        backupProviderEnabled: Boolean,
        syncPackageInvalid: Boolean,
        folderSyncAccessUnavailable: Boolean,
        syncGuardianPath: String,
        syncGuardianSha256: String,
        syncWorkerPath: String,
        syncWorkerSha256: String,
        syncRuntimeDirectory: String,
    ): String

    @JvmStatic
    private external fun nativeStop(handle: Long): String

    @JvmStatic
    private external fun nativeState(handle: Long): String

    /**
     * Starts the process-local node.
     *
     * [keyProtectionLevel] carries the measured Android Keystore capability across the
     * boundary; the Rust side decodes it with `IdentityProtection::from_wire` and refuses
     * to start on [KeyProtectionLevel.UNAVAILABLE] or on any value it does not recognise.
     * [keyEncryptionKey] is exactly 32 transient bytes for [keyVersion]. Native code clears
     * both JVM secret arrays immediately, installs a zeroizing `StaticKeyProtector` in
     * `NodeRuntimeConfig`, and rejects missing, malformed, or unversioned material.
     */
    fun start(
        dataDirectory: String,
        deviceName: String,
        lanDiscoveryEnabled: Boolean,
        apiToken: ByteArray,
        keyEncryptionKey: ByteArray,
        keyVersion: Int,
        maximumTotalBytes: Long,
        freeSpaceReserveBytes: Long,
        keyProtectionLevel: KeyProtectionLevel,
        syncEngine: PackagedSyncEnginePackage,
        backupProviderEnabled: Boolean = true,
        folderSyncAccessUnavailable: Boolean = false,
        folderSyncListenerPort: Int = FOLDER_SYNC_LISTENER_PORT,
    ): NativeNodeResponse {
        if (!libraryLoaded) return NativeNodeResponse.unavailable()
        require(BuildConfig.DEBUG || folderSyncListenerPort == FOLDER_SYNC_LISTENER_PORT) {
            "Release builds use Covalent's fixed folder-sync listener port."
        }
        return parse(runCatching {
            val packaged = syncEngine.verifiedOrNull()
            nativeStart(
                dataDirectory,
                deviceName,
                lanDiscoveryEnabled,
                apiToken,
                keyEncryptionKey,
                keyVersion,
                maximumTotalBytes,
                freeSpaceReserveBytes,
                keyProtectionLevel.wireValue,
                backupProviderEnabled,
                syncEngine is PackagedSyncEnginePackage.Invalid,
                folderSyncAccessUnavailable,
                packaged?.guardianPath.orEmpty(),
                packaged?.guardianSha256.orEmpty(),
                packaged?.workerPath.orEmpty(),
                packaged?.workerSha256.orEmpty(),
                packaged?.runtimeDirectory.orEmpty(),
                folderSyncListenerPort,
            )
        }.getOrElse { NativeNodeResponse.unavailable().toJson() })
    }

    /**
     * Recovers a node identity into its ordinary private data root before normal opening.
     * Native code clears all four JVM byte arrays immediately. The caller also clears its
     * arrays in `finally`, so validation and linkage failures cannot leave an owned copy here.
     */
    fun recoverStart(
        dataDirectory: String,
        deviceName: String,
        lanDiscoveryEnabled: Boolean,
        apiToken: ByteArray,
        keyEncryptionKey: ByteArray,
        keyVersion: Int,
        maximumTotalBytes: Long,
        freeSpaceReserveBytes: Long,
        keyProtectionLevel: KeyProtectionLevel,
        recoveryKit: ByteArray,
        recoveryKey: ByteArray,
        syncEngine: PackagedSyncEnginePackage,
        backupProviderEnabled: Boolean = true,
        folderSyncAccessUnavailable: Boolean = false,
    ): NativeNodeResponse {
        if (!libraryLoaded) return NativeNodeResponse.unavailable()
        return parse(runCatching {
            val packaged = syncEngine.verifiedOrNull()
            nativeRecoverStart(
                dataDirectory,
                deviceName,
                lanDiscoveryEnabled,
                apiToken,
                keyEncryptionKey,
                keyVersion,
                maximumTotalBytes,
                freeSpaceReserveBytes,
                keyProtectionLevel.wireValue,
                recoveryKit,
                recoveryKey,
                backupProviderEnabled,
                syncEngine is PackagedSyncEnginePackage.Invalid,
                folderSyncAccessUnavailable,
                packaged?.guardianPath.orEmpty(),
                packaged?.guardianSha256.orEmpty(),
                packaged?.workerPath.orEmpty(),
                packaged?.workerSha256.orEmpty(),
                packaged?.runtimeDirectory.orEmpty(),
            )
        }.getOrElse { NativeNodeResponse.unavailable().toJson() })
    }

    fun stop(handle: Long): NativeNodeResponse =
        if (!libraryLoaded) NativeNodeResponse.unavailable()
        else parse(runCatching { nativeStop(handle) }.getOrElse { NativeNodeResponse.unavailable().toJson() })

    fun state(handle: Long): NativeNodeResponse =
        if (!libraryLoaded) NativeNodeResponse.unavailable()
        else parse(runCatching { nativeState(handle) }.getOrElse { NativeNodeResponse.unavailable().toJson() })

    private fun parse(value: String): NativeNodeResponse = runCatching {
        JSONObject(value).let { objectValue ->
            NativeNodeResponse(
                ok = objectValue.optBoolean("ok", false),
                code = objectValue.optString("code", "native_response_invalid"),
                message = objectValue.optString("message", "On-phone backup storage is unavailable."),
                handle = objectValue.optLong("handle", 0L).takeIf { it > 0L },
                apiBaseUrl = objectValue.optString("apiBaseUrl").takeIf(String::isNotBlank),
                peerAddress = objectValue.optString("peerAddress").takeIf(String::isNotBlank),
                state = objectValue.optString("state", "stopped"),
            )
        }
    }.getOrElse { NativeNodeResponse.unavailable() }

    private const val FOLDER_SYNC_LISTENER_PORT = 8_789
}

private fun PackagedSyncEnginePackage.verifiedOrNull(): VerifiedPackagedSyncEngine? =
    (this as? PackagedSyncEnginePackage.Verified)?.engine

internal data class NativeNodeResponse(
    val ok: Boolean,
    val code: String,
    val message: String,
    val handle: Long?,
    val apiBaseUrl: String?,
    val peerAddress: String?,
    val state: String,
) {
    fun toJson(): String = JSONObject().apply {
        put("ok", ok)
        put("code", code)
        put("message", message)
        put("handle", handle)
        put("apiBaseUrl", apiBaseUrl)
        put("peerAddress", peerAddress)
        put("state", state)
    }.toString()

    companion object {
        fun unavailable() = NativeNodeResponse(
            ok = false,
            code = "native_runtime_unavailable",
            message = "On-phone backup storage is unavailable on this device.",
            handle = null,
            apiBaseUrl = null,
            peerAddress = null,
            state = "stopped",
        )
    }
}
