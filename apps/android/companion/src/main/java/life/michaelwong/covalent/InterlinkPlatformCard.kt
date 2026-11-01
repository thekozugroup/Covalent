package life.michaelwong.covalent

import android.text.format.Formatter
import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import java.text.NumberFormat
import life.michaelwong.covalent.appliance.*

@Composable
internal fun InterlinkPlatformCard(state: ApplianceDashboardState) {
    if (state.connection != ApplianceConnectionState.CONNECTED) return
    val platform = state.platform
    val snapshot = platform?.snapshot
    Card(Modifier.fillMaxWidth().testTag("interlink-platform-card")) {
        Column(Modifier.padding(20.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Text(stringResource(R.string.interlink_storage_title), style = MaterialTheme.typography.titleLarge,
                modifier = Modifier.semantics { heading() })
            if (snapshot == null) {
                Text(stringResource(if (platform?.supported == false) R.string.interlink_platform_unsupported else R.string.interlink_platform_unavailable))
                return@Column
            }
            val context = LocalContext.current
            Text(snapshot.spaceAfterReserve?.let {
                stringResource(R.string.interlink_storage_space, Formatter.formatFileSize(context, it))
            } ?: stringResource(R.string.interlink_storage_space_unknown))
            if (!snapshot.volumesAvailable) Text(stringResource(R.string.interlink_volumes_unavailable))
            if (!snapshot.camerasAvailable) Text(stringResource(R.string.interlink_cameras_unavailable))
            if (snapshot.sources.isEmpty() && snapshot.volumesAvailable && snapshot.camerasAvailable) Text(stringResource(R.string.interlink_storage_empty))
            if (snapshot.truncated) Text(stringResource(R.string.interlink_storage_truncated))
            snapshot.sources.forEach { source ->
                OutlinedCard(Modifier.fillMaxWidth().testTag("interlink-source-${source.id}")) {
                    Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                        Text(source.name, style = MaterialTheme.typography.titleMedium)
                        Text(stringResource(if (!source.connected) R.string.interlink_source_disconnected
                            else if (source.camera) R.string.interlink_source_camera else R.string.interlink_source_drive),
                            style = MaterialTheme.typography.labelLarge)
                        Text(stringResource(R.string.interlink_source_destination), style = MaterialTheme.typography.labelMedium)
                        Text(source.destination, style = MaterialTheme.typography.bodySmall)
                        if (source.count != null && source.bytes != null) {
                            Text(stringResource(R.string.interlink_source_counts, NumberFormat.getIntegerInstance().format(source.count),
                                Formatter.formatFileSize(context, source.bytes)), modifier = Modifier.testTag("interlink-source-counts"))
                            Text(stringResource(if (source.preview == PiPreviewState.INCOMPLETE) R.string.interlink_preview_incomplete
                                else R.string.interlink_preview_cached), style = MaterialTheme.typography.bodySmall)
                        } else Text(stringResource(when (source.preview) {
                            PiPreviewState.PENDING, PiPreviewState.SCANNING -> R.string.interlink_preview_scanning
                            else -> R.string.interlink_preview_unavailable
                        }))
                        Text(stringResource(if (source.automatic) R.string.interlink_source_automatic else R.string.interlink_source_manual),
                            style = MaterialTheme.typography.bodySmall)
                    }
                }
            }
            Text(stringResource(R.string.interlink_import_approval_unavailable), style = MaterialTheme.typography.bodySmall)
            HorizontalDivider()
            Text(stringResource(R.string.interlink_updates_title), style = MaterialTheme.typography.titleMedium)
            val updates = snapshot.updates
            Text(stringResource(when {
                updates == null -> R.string.interlink_updates_unavailable
                !updates.configured -> R.string.interlink_updates_unconfigured
                updates.busy -> R.string.interlink_updates_busy
                updates.automatic -> R.string.interlink_updates_automatic
                else -> R.string.interlink_updates_paused
            }))
        }
    }
}
