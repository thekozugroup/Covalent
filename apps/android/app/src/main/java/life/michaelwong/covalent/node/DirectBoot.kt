package life.michaelwong.covalent.node

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.SharedPreferences
import android.os.UserManager
import android.util.Log
import androidx.core.content.ContextCompat
import androidx.core.content.edit

/**
 * Android's Direct Boot boundary, expressed once.
 *
 * Between a reboot and the user's first unlock, Android keeps *credential-encrypted* (CE)
 * storage sealed. `Context.getSharedPreferences` is CE by default, and in that window it
 * does not return an empty file — it throws:
 *
 * ```
 * IllegalStateException: SharedPreferences in credential encrypted storage
 * are not available until after user (id 0) is unlocked
 * ```
 *
 * So any read that happens while the process is alive in that window crashes, and a read
 * from `Application.onCreate` takes the whole process down with it. A locked phone is
 * exactly the state a device is in when it reboots unattended and when a scheduled backup
 * fires, so this is not a corner case.
 *
 * ## Why nothing moves to device-protected storage
 *
 * The obvious-looking alternative is `createDeviceProtectedStorageContext()`, whose
 * storage *is* readable before unlock. Covalent deliberately keeps nothing there, and this
 * is a security decision rather than an omission:
 *
 *  * Device-protected storage is readable by whoever holds a **locked** phone. It must
 *    therefore never hold the local API token envelope, the node's TLS identity, or the
 *    address of a paired node.
 *  * Moving only the non-secret flags there would buy nothing anyway. The node's whole
 *    state directory is `noBackupFilesDir`, which is credential-encrypted, and its API
 *    token envelope lives in a credential-encrypted preference file. The node cannot start
 *    before unlock no matter which flags are readable.
 *  * It would cost something. An `enabled` flag readable from a locked phone announces
 *    that this device is a Covalent backup target — an inventory signal handed to an
 *    attacker for no functional gain.
 *
 * The rule is therefore: read nothing before unlock, wait for it, and meanwhile say
 * plainly that this is what is happening.
 */
internal object DirectBoot {
    private const val TAG = "CovalentDirectBoot"

    /**
     * True once credential-encrypted storage is readable for this user.
     *
     * A context with no `UserManager` — only reachable from a stripped-down test double —
     * is treated as unlocked, so the storage call that follows either succeeds or fails
     * where the caller can see it, rather than being silently skipped.
     */
    fun isUserUnlocked(context: Context): Boolean =
        context.getSystemService(UserManager::class.java)?.isUserUnlocked ?: true

    /**
     * Runs [action] now if this user is already unlocked, otherwise exactly once when they
     * unlock.
     *
     * Registering the receiver reads no storage, so this is safe to call from
     * `Application.onCreate` in any boot state. `ACTION_USER_UNLOCKED` is a protected
     * system broadcast, so the receiver is registered not-exported: nothing but the system
     * can deliver it.
     */
    fun whenUserUnlocked(context: Context, action: () -> Unit) {
        val applicationContext = context.applicationContext
        if (isUserUnlocked(applicationContext)) {
            action()
            return
        }
        Log.i(TAG, "Credential-encrypted storage is sealed; waiting for this user to unlock.")
        val receiver = object : BroadcastReceiver() {
            override fun onReceive(receivedIn: Context, intent: Intent) {
                if (intent.action != Intent.ACTION_USER_UNLOCKED) return
                // Unregister before acting: the action opens storage and may itself fail,
                // and a receiver left registered would repeat the attempt on every unlock.
                runCatching { applicationContext.unregisterReceiver(this) }
                action()
            }
        }
        ContextCompat.registerReceiver(
            applicationContext,
            receiver,
            IntentFilter(Intent.ACTION_USER_UNLOCKED),
            ContextCompat.RECEIVER_NOT_EXPORTED,
        )
    }
}

/**
 * A credential-encrypted preference file that reports "sealed" instead of throwing.
 *
 * Reads fall back to the caller's default and writes are dropped while storage is sealed,
 * which is safe **only** because [readable] is checked at every decision point that could
 * otherwise mistake "unknown" for "off": the embedded node refuses to start, refuses to be
 * enabled, and reports [KeyProtectionLevel.UNAVAILABLE] rather than acting on a default it
 * did not read. Nothing here degrades to running the node unprotected.
 *
 * The handle is resolved lazily and only a success is remembered, so an instance created
 * during the locked window starts working after the user unlocks instead of staying
 * permanently blind.
 */
internal class CredentialProtectedPreferences(context: Context, private val name: String) {
    private val applicationContext = context.applicationContext

    @Volatile
    private var opened: SharedPreferences? = null

    /** False only while this user's credential-encrypted storage is still sealed. */
    val readable: Boolean get() = delegate() != null

    fun getBoolean(key: String, fallback: Boolean): Boolean =
        delegate()?.getBoolean(key, fallback) ?: fallback

    fun getLong(key: String, fallback: Long): Long =
        delegate()?.getLong(key, fallback) ?: fallback

    fun getString(key: String, fallback: String?): String? =
        delegate()?.getString(key, fallback) ?: fallback

    fun edit(block: SharedPreferences.Editor.() -> Unit) {
        delegate()?.edit(action = block)
    }

    private fun delegate(): SharedPreferences? {
        opened?.let { return it }
        if (!DirectBoot.isUserUnlocked(applicationContext)) return null
        // The platform can still refuse the volume after `isUserUnlocked` turns true —
        // around the unlock itself, and for any context standing in for a locked device —
        // so the throw is caught here rather than left to reach the caller.
        return runCatching { applicationContext.getSharedPreferences(name, Context.MODE_PRIVATE) }
            .onFailure { failure ->
                Log.i(TAG, "$name is not readable yet: ${failure.javaClass.simpleName}")
            }
            .getOrNull()
            ?.also { opened = it }
    }

    private companion object {
        const val TAG = "CovalentDirectBoot"
    }
}
