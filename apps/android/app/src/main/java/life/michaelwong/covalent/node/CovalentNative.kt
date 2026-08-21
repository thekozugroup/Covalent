package life.michaelwong.covalent.node

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
        maximumTotalBytes: Long,
        freeSpaceReserveBytes: Long,
        keyProtectionLevel: Int,
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
     * The policy therefore lives on one side and the measurement on the other, so neither
     * can quietly assume the answer.
     */
    fun start(
        dataDirectory: String,
        deviceName: String,
        lanDiscoveryEnabled: Boolean,
        apiToken: ByteArray,
        maximumTotalBytes: Long,
        freeSpaceReserveBytes: Long,
        keyProtectionLevel: KeyProtectionLevel,
    ): NativeNodeResponse {
        if (!libraryLoaded) return NativeNodeResponse.unavailable()
        return parse(runCatching {
            nativeStart(
                dataDirectory,
                deviceName,
                lanDiscoveryEnabled,
                apiToken,
                maximumTotalBytes,
                freeSpaceReserveBytes,
                keyProtectionLevel.wireValue,
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
}

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
