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

@OptIn(ExperimentalLayoutApi::class)
@Composable
internal fun InterlinkPlatformCard(
    state: ApplianceDashboardState, imports: PiImportsState? = null, busy: Boolean = false,
    review: (String, Boolean) -> Unit = { _, _ -> },
) {
    if (state.connection != ApplianceConnectionState.CONNECTED) return
    val platform = state.platform
    val snapshot = platform?.snapshot
    val approval = imports?.observation?.context?.takeIf {
        imports.actionsAvailable && imports.selected == state.selection && it.bond.appliance == state.selection?.appliance &&
            it.bond.controller == state.selection?.local && it.bond.nativeGeneration == state.observation?.bondGeneration
    }
    Card(Modifier.fillMaxWidth().testTag("interlink-platform-card")) {
        Column(Modifier.padding(20.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Text(stringResource(R.string.interlink_storage_title), style = MaterialTheme.typography.titleLarge,
                modifier = Modifier.semantics { heading() })
            if (snapshot == null && approval == null) {
                Text(stringResource(if (platform?.supported == false) R.string.interlink_platform_unsupported else R.string.interlink_platform_unavailable))
                return@Column
            }
            val context = LocalContext.current
            val space = if (approval != null) approval.spaceAfterReserve else snapshot?.spaceAfterReserve
            Text(space?.let {
                stringResource(R.string.interlink_storage_space, Formatter.formatFileSize(context, it))
            } ?: stringResource(R.string.interlink_storage_space_unknown))
            val volumes = approval?.volumesAvailable ?: (snapshot?.volumesAvailable == true)
            val cameras = approval?.camerasAvailable ?: (snapshot?.camerasAvailable == true)
            val sources = if (approval == null) snapshot?.sources.orEmpty() else
                approval.sources.map { it.source } + snapshot?.sources.orEmpty().filter { source ->
                    source.camera && approval.camerasAvailable == null && approval.sources.none { it.source.id == source.id }
                }
            if (!volumes) Text(stringResource(R.string.interlink_volumes_unavailable))
            if (!cameras) Text(stringResource(R.string.interlink_cameras_unavailable))
            if (sources.isEmpty() && volumes && cameras) Text(stringResource(R.string.interlink_storage_empty))
            if ((approval?.truncated ?: false) || snapshot?.truncated == true) Text(stringResource(R.string.interlink_storage_truncated))
            sources.forEach { source ->
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
                        val control = approval?.sources?.singleOrNull { it.source.id == source.id }
                        if (control != null) {
                            val pending = imports?.actions.orEmpty().filter { !it.resolved && it.choice.bond == approval?.bond && it.choice.camera == source.camera }
                            FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                                Button(enabled = !busy && pending.isEmpty() && control.confirmation != null,
                                    onClick = { review(source.id, false) }, modifier = Modifier.testTag("interlink-import-review-${source.id}")) {
                                    Text(stringResource(R.string.interlink_import_review))
                                }
                                if (source.automatic || pending.any { it.choice.sourceId == source.id }) TextButton(enabled = !busy,
                                    onClick = { review(source.id, true) }, modifier = Modifier.testTag("interlink-import-pause-${source.id}")) {
                                    Text(stringResource(R.string.interlink_import_pause))
                                }
                            }
                            if (pending.isNotEmpty()) Text(stringResource(R.string.interlink_import_pending_detail), style = MaterialTheme.typography.bodySmall)
                            else if (control.confirmation == null) Text(stringResource(R.string.interlink_import_preview_wait), style = MaterialTheme.typography.bodySmall)
                        }
                        if (source.camera && control == null) Text(stringResource(R.string.interlink_camera_approval_unavailable), style = MaterialTheme.typography.bodySmall)
                    }
                }
            }
            if (imports != null && !imports.actionsAvailable) Text(stringResource(R.string.interlink_import_history_unavailable), style = MaterialTheme.typography.bodySmall)
            else if (approval == null) Text(stringResource(R.string.interlink_import_approval_unavailable), style = MaterialTheme.typography.bodySmall)
        }
    }
}
