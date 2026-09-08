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

    fun renew(offerId: String): FolderSyncMutation {
        check(grants.records().any { it.offerId == offerId && it.kind == FolderSyncGrantKind.OFFER }) {
            "The saved folder choice for this outgoing invitation is unavailable."
        }
        val connection = ensureNodeReady()
        val mutation = api.renew(connection, offerId)
        check(mutation.offerId != null && mutation.offerId != offerId) {
            "The node did not identify a new invitation."
        }
        val snapshot = api.status(connection)
        check(snapshot.shares.any {
            !it.incoming && it.offerId == mutation.offerId && offerId in it.supersededOfferIds
        }) { "The node did not confirm the replacement invitation. Refresh folders before trying again." }
        // The original binding remains durable if the response or this save
        // fails. Refresh recovers it from the authenticated ID relationship.
        grants.reconcile(snapshot)
        return mutation
    }

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
