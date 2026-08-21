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
    ): String

    @JvmStatic
    private external fun nativeStop(handle: Long): String

    @JvmStatic
    private external fun nativeState(handle: Long): String

    fun start(
        dataDirectory: String,
        deviceName: String,
        lanDiscoveryEnabled: Boolean,
        apiToken: ByteArray,
        maximumTotalBytes: Long,
        freeSpaceReserveBytes: Long,
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
                message = objectValue.optString("message", "Embedded provider is unavailable."),
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
            message = "Embedded provider is unavailable on this device.",
            handle = null,
            apiBaseUrl = null,
            peerAddress = null,
            state = "stopped",
        )
    }
}
