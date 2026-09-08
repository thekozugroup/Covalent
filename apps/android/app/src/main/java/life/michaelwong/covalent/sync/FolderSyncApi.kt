package life.michaelwong.covalent.sync

import java.util.UUID
import life.michaelwong.covalent.data.CovalentNodeClient
import life.michaelwong.covalent.model.FolderSyncMutation
import life.michaelwong.covalent.model.FolderSyncStatus
import life.michaelwong.covalent.model.NodeConnection

internal interface FolderSyncApi {
    fun status(connection: NodeConnection): FolderSyncStatus
    fun offer(
        connection: NodeConnection,
        peerId: String,
        folderId: UUID,
        label: String,
        selectedRoot: String,
    ): FolderSyncMutation
    fun accept(connection: NodeConnection, offerId: String, selectedRoot: String): FolderSyncMutation
    fun pause(connection: NodeConnection, offerId: String, paused: Boolean): FolderSyncMutation
    fun remove(connection: NodeConnection, offerId: String): FolderSyncMutation
    fun repair(connection: NodeConnection, offerId: String, selectedRoot: String): FolderSyncMutation
    fun retry(connection: NodeConnection): FolderSyncMutation
}

internal class NodeFolderSyncApi(private val client: CovalentNodeClient) : FolderSyncApi {
    override fun status(connection: NodeConnection) =
        client.folderSyncStatus(connection.baseUrl, connection.token)

    override fun offer(
        connection: NodeConnection,
        peerId: String,
        folderId: UUID,
        label: String,
        selectedRoot: String,
    ) = client.offerFolder(connection.baseUrl, connection.token, peerId, folderId, label, selectedRoot)

    override fun accept(connection: NodeConnection, offerId: String, selectedRoot: String) =
        client.acceptFolder(connection.baseUrl, connection.token, offerId, selectedRoot)

    override fun pause(connection: NodeConnection, offerId: String, paused: Boolean) =
        client.pauseFolder(connection.baseUrl, connection.token, offerId, paused)

    override fun remove(connection: NodeConnection, offerId: String) =
        client.removeFolder(connection.baseUrl, connection.token, offerId)

    override fun repair(connection: NodeConnection, offerId: String, selectedRoot: String) =
        client.repairFolder(connection.baseUrl, connection.token, offerId, selectedRoot)

    override fun retry(connection: NodeConnection) =
        client.retryFolderSync(connection.baseUrl, connection.token)
}
