package life.michaelwong.covalent.sync

import java.util.UUID
import life.michaelwong.covalent.model.FolderSyncMutation
import life.michaelwong.covalent.model.NodeConnection

/** Orders private capability persistence before any authenticated sharing mutation. */
internal class FolderSyncActions(
    private val grants: FolderSyncGrantJournal,
    private val api: FolderSyncApi,
    private val ensureNodeReady: () -> NodeConnection,
) {
    fun offer(peerId: String, folderId: UUID, label: String, root: String): FolderSyncMutation {
        val durableFolderId = grants.prepareOffer(peerId, folderId, root, label)
        val connection = ensureNodeReady()
        return api.offer(connection, peerId, durableFolderId, label, root).also { mutation ->
            grants.finishOffer(durableFolderId, checkNotNull(mutation.offerId) { "The node omitted the folder offer ID." })
        }
    }

    fun accept(offerId: String, root: String): FolderSyncMutation {
        grants.prepareAcceptance(offerId, root)
        return api.accept(ensureNodeReady(), offerId, root)
    }

    fun pause(offerId: String, paused: Boolean): FolderSyncMutation =
        api.pause(ensureNodeReady(), offerId, paused)

    fun remove(offerId: String): FolderSyncMutation =
        api.remove(ensureNodeReady(), offerId).also { grants.remove(offerId) }

    fun retry(): FolderSyncMutation = api.retry(ensureNodeReady())
}
