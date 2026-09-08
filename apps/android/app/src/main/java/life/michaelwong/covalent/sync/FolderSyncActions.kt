package life.michaelwong.covalent.sync

import java.util.UUID
import life.michaelwong.covalent.model.FolderSyncMutation
import life.michaelwong.covalent.model.NodeConnection

/** Orders private capability persistence before any authenticated sharing mutation. */
internal class FolderSyncActions(
    private val grants: FolderSyncGrantJournal,
    private val api: FolderSyncApi,
    private val ensureNodeReady: () -> NodeConnection,
    private val validateRoot: (String) -> String,
    private val onRepairAcknowledged: () -> Unit,
) {
    fun offer(peerId: String, folderId: UUID, label: String, root: String): FolderSyncMutation {
        val selectedRoot = validateRoot(root)
        val durableFolderId = grants.prepareOffer(peerId, folderId, selectedRoot, label)
        val connection = ensureNodeReady()
        return api.offer(connection, peerId, durableFolderId, label, selectedRoot).also { mutation ->
            grants.finishOffer(durableFolderId, checkNotNull(mutation.offerId) { "The node omitted the folder offer ID." })
        }
    }

    fun accept(offerId: String, root: String): FolderSyncMutation {
        val selectedRoot = validateRoot(root)
        grants.prepareAcceptance(offerId, selectedRoot)
        return api.accept(ensureNodeReady(), offerId, selectedRoot)
    }

    fun pause(offerId: String, paused: Boolean): FolderSyncMutation =
        api.pause(ensureNodeReady(), offerId, paused)

    fun repair(offerId: String, root: String): FolderSyncMutation {
        val selectedRoot = validateRoot(root)
        grants.prepareRepair(offerId, selectedRoot)
        return api.repair(ensureNodeReady(), offerId, selectedRoot).also { mutation ->
            requireMutationOffer(mutation, offerId)
            grants.finishRepair(offerId, selectedRoot)
            onRepairAcknowledged()
        }
    }

    fun retryRepair(offerId: String): FolderSyncMutation = repair(
        offerId,
        checkNotNull(grants.pendingRepairRoot(offerId)) { "There is no saved folder repair to retry." },
    )

    fun remove(offerId: String): FolderSyncMutation {
        grants.prepareRemoval(offerId)
        return api.remove(ensureNodeReady(), offerId).also { mutation ->
            requireMutationOffer(mutation, offerId)
            grants.finishRemoval(offerId)
        }
    }

    fun retry(): FolderSyncMutation = api.retry(ensureNodeReady())

    private fun requireMutationOffer(mutation: FolderSyncMutation, expected: String) {
        check(mutation.offerId == expected) { "The node acknowledged a different folder share." }
    }
}
