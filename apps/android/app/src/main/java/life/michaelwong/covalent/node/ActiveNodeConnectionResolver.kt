package life.michaelwong.covalent.node

import android.content.Context
import life.michaelwong.covalent.data.SecureNodeStore
import life.michaelwong.covalent.model.NodeConnection

/**
 * Resolves the deliberately selected controller without ever rewriting the
 * separately persisted external-node configuration.
 *
 * Callers must treat a missing result as disconnected; the resolver never
 * silently falls back from a selected local node to an external node.
 */
class ActiveNodeConnectionResolver(context: Context) {
    private val manager = EmbeddedNodeManager(context.applicationContext)

    fun activeConnection(externalStore: SecureNodeStore): NodeConnection? = when (manager.activeMode()) {
        NodeMode.LOCAL -> manager.localConnectionForActiveMode()
        NodeMode.EXTERNAL -> externalStore.connectionOrNull()
    }

    fun isLocalActive(): Boolean = manager.activeMode() == NodeMode.LOCAL
}

private fun SecureNodeStore.connectionOrNull(): NodeConnection? =
    baseUrl.takeIf(String::isNotBlank)?.let { address ->
        token.takeIf(String::isNotBlank)?.let { bearer -> NodeConnection(address, bearer) }
    }
