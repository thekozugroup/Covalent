package life.michaelwong.covalent.node

import android.content.Context
import android.util.Base64
import java.security.SecureRandom
import java.util.UUID
import life.michaelwong.covalent.sync.SafFolderGrantStore
import life.michaelwong.covalent.sync.SafWebDavServer

/** Owns process-local WebDAV credentials and binds persisted SAF grants to one native runtime. */
internal object AndroidSafGrantCoordinator {
    private data class ActiveGrant(val id: UUID, val server: SafWebDavServer)

    private val random = SecureRandom()
    private var activeHandle: Long? = null
    private var active = emptyList<ActiveGrant>()

    @Synchronized
    fun registerPersisted(context: Context, handle: Long): Boolean {
        require(handle > 0)
        closeLocked(activeHandle)
        val started = mutableListOf<ActiveGrant>()
        for (grant in SafFolderGrantStore(context.applicationContext).allAccessible()) {
            val username = credential(18)
            val password = credential(32)
            val server = SafWebDavServer(context.applicationContext, grant.treeUri, username, password)
            val endpoint = runCatching { server.start() }.getOrElse {
                server.close()
                started.forEach {
                    CovalentNative.unregisterFolderGrant(handle, it.id.toString())
                    it.server.close()
                }
                return false
            }
            if (
                CovalentNative.registerFolderGrant(
                    handle,
                    grant.id.toString(),
                    endpoint.port,
                    endpoint.username,
                    endpoint.password,
                ) != NativeFolderGrantResult.OK
            ) {
                server.close()
                started.forEach {
                    CovalentNative.unregisterFolderGrant(handle, it.id.toString())
                    it.server.close()
                }
                return false
            }
            started += ActiveGrant(grant.id, server)
        }
        activeHandle = handle
        active = started
        return true
    }

    @Synchronized
    fun close(handle: Long) {
        if (activeHandle == handle) closeLocked(handle)
    }

    private fun closeLocked(handle: Long?) {
        if (handle != null) active.forEach {
            CovalentNative.unregisterFolderGrant(handle, it.id.toString())
        }
        active.forEach { it.server.close() }
        active = emptyList()
        activeHandle = null
    }

    private fun credential(bytes: Int): String {
        val raw = ByteArray(bytes)
        return try {
            random.nextBytes(raw)
            Base64.encodeToString(raw, Base64.URL_SAFE or Base64.NO_WRAP or Base64.NO_PADDING)
        } finally {
            raw.fill(0)
        }
    }
}
