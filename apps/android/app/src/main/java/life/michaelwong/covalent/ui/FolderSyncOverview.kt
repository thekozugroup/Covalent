package life.michaelwong.covalent.ui

import androidx.annotation.StringRes
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import life.michaelwong.covalent.R
import life.michaelwong.covalent.model.FolderSharePhase
import life.michaelwong.covalent.model.FolderSyncAvailability
import life.michaelwong.covalent.model.FolderSyncLifecycle
import life.michaelwong.covalent.model.FolderSyncStatus

/** Setup is durable; stopped workers do not make a saved link unconfigured. */
@StringRes
internal fun FolderSyncStatus.overviewResource(
    photoAppliance: Boolean,
    pendingRepairOffers: Set<String>,
): Int {
    val links = shares.filter { it.phase != FolderSharePhase.REMOVED || it.remoteRemovalPending }
    val configured = links.filter { it.phase != FolderSharePhase.REMOVED }
    return when {
        issue != null || availability == FolderSyncAvailability.NEEDS_ATTENTION ||
            lifecycle == FolderSyncLifecycle.NEEDS_ATTENTION ->
            R.string.folder_sync_connection_attention
        lifecycle == FolderSyncLifecycle.INITIAL_SCANNING -> R.string.folder_sync_checking_folders
        lifecycle == FolderSyncLifecycle.STILL_STOPPING -> R.string.folder_sync_stopping
        photoAppliance && configured.any { link ->
            link.pairingUpgradeRequired || link.offerId in pendingRepairOffers ||
                folders.any { it.folderId == link.folderId && it.accessUnavailable }
        } -> R.string.folder_sync_connection_attention
        photoAppliance && configured.any { it.incoming && it.phase == FolderSharePhase.OFFERED && !it.expired } ->
            R.string.interlink_destination_next
        photoAppliance && configured.isNotEmpty() -> when {
            configured.any { it.expired || it.phase in setOf(FolderSharePhase.OFFERED, FolderSharePhase.AWAITING_COMMIT) } ->
                R.string.interlink_links_pending
            configured.all { it.phase == FolderSharePhase.PAUSED || it.linkSettings?.settings?.paused == true } ->
                R.string.interlink_links_paused
            else -> R.string.interlink_links_ready
        }
        photoAppliance && links.isNotEmpty() -> R.string.interlink_links_removing
        photoAppliance && peers.isEmpty() -> R.string.interlink_pair_next
        photoAppliance && folders.isEmpty() -> R.string.interlink_folder_next
        lifecycle == FolderSyncLifecycle.RUNNING -> R.string.folder_sync_running
        else -> R.string.folder_sync_ready_to_share
    }
}

@Composable
internal fun FolderSyncOverview(
    status: FolderSyncStatus?,
    photoAppliance: Boolean,
    pendingRepairOffers: Set<String>,
    busy: Boolean,
    refresh: () -> Unit,
) {
    val message = stringResource(status?.overviewResource(photoAppliance, pendingRepairOffers)
        ?: R.string.folder_sync_starting)
    BoxWithConstraints(Modifier.fillMaxWidth()) {
        if (maxWidth < 320.dp || LocalDensity.current.fontScale >= 1.3f) {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(message, modifier = Modifier.testTag("folder-sync-overview"))
                OverviewRefresh(busy, refresh)
            }
        } else {
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(12.dp),
                verticalAlignment = Alignment.CenterVertically) {
                Text(message, modifier = Modifier.weight(1f).testTag("folder-sync-overview"))
                OverviewRefresh(busy, refresh)
            }
        }
    }
}

@Composable
private fun OverviewRefresh(busy: Boolean, refresh: () -> Unit) {
    OutlinedButton(onClick = refresh, enabled = !busy) { Text(stringResource(R.string.action_refresh)) }
}
