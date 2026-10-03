package life.michaelwong.covalent.node

import java.io.Closeable
import life.michaelwong.covalent.data.RecoveryBootstrapMaterial

/**
 * Process-local, single-consumer bridge from the foreground activity to the service that owns
 * the native handle. Nothing in this object is parcelled, saved, logged, or restarted by Android.
 */
internal class EmbeddedRecoveryRequest(
    val material: RecoveryBootstrapMaterial,
    val maximumTotalBytes: Long,
    val freeSpaceReserveBytes: Long,
    val lanDiscoveryRequested: Boolean,
) : Closeable {
    override fun close() = material.close()
    override fun toString(): String = "EmbeddedRecoveryRequest([REDACTED])"
}

internal object RecoveryBootstrapHandoff {
    private var pending: EmbeddedRecoveryRequest? = null
    private var active = false

    @Synchronized
    fun offer(request: EmbeddedRecoveryRequest): Boolean {
        if (active) return false
        pending = request
        active = true
        return true
    }

    @Synchronized
    fun take(): EmbeddedRecoveryRequest? = pending.also { pending = null }

    @Synchronized
    fun cancel() {
        pending?.close()
        pending = null
        active = false
    }

    /** Cancels only a request the service has not consumed; an in-flight JNI call owns its input. */
    @Synchronized
    fun cancelPending() {
        if (pending == null) return
        pending?.close()
        pending = null
        active = false
    }

    @Synchronized
    fun finish() {
        pending?.close()
        pending = null
        active = false
    }

    @Synchronized
    fun isActive(): Boolean = active
}
