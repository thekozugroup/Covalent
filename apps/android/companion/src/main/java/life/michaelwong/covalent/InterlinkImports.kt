package life.michaelwong.covalent

import android.text.format.Formatter
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import java.text.NumberFormat
import life.michaelwong.covalent.appliance.*

@Composable
internal fun InterlinkImportConfirmation(review: PiImportReview, busy: Boolean, dismiss: () -> Unit, confirm: (PiImportMode) -> Unit) {
    var rememberSource by remember(review.id) { mutableStateOf(review.source.canRemember && review.source.source.automatic) }
    var submitted by remember(review.id) { mutableStateOf(false) }
    var folderDetails by remember(review.id) { mutableStateOf(false) }
    val context = LocalContext.current
    AlertDialog(
        onDismissRequest = { if (!busy && !submitted) dismiss() },
        title = { Text(if (review.pause) stringResource(R.string.interlink_import_pause_title, review.source.source.name) else stringResource(R.string.interlink_import_title)) },
        text = {
            Column(Modifier.verticalScroll(rememberScrollState()).testTag("interlink-import-review"), verticalArrangement = Arrangement.spacedBy(12.dp)) {
                if (review.pause) Text(stringResource(R.string.interlink_import_pause_detail))
                else {
                    Text(review.source.source.name, style = MaterialTheme.typography.titleMedium)
                    Text(stringResource(R.string.interlink_source_counts, NumberFormat.getIntegerInstance().format(review.source.source.count),
                        Formatter.formatFileSize(context, checkNotNull(review.source.source.bytes))), style = MaterialTheme.typography.bodyLarge,
                        modifier = Modifier.testTag("interlink-import-review-counts"))
                    Text(stringResource(R.string.interlink_import_destination, review.selected.name), modifier = Modifier.testTag("interlink-import-destination"))
                    Text(review.context.spaceAfterReserve?.let { stringResource(R.string.interlink_import_space, Formatter.formatFileSize(context, it)) }
                        ?: stringResource(R.string.interlink_import_space_unknown))
                    Text(stringResource(R.string.interlink_import_counts_detail), style = MaterialTheme.typography.bodySmall)
                    TextButton(enabled = !busy && !submitted, onClick = { folderDetails = !folderDetails },
                        modifier = Modifier.testTag("interlink-import-folder-details"), contentPadding = PaddingValues(0.dp)) {
                        Text(stringResource(R.string.interlink_import_folder_details))
                    }
                    if (folderDetails) Text(review.source.source.destination, style = MaterialTheme.typography.bodySmall,
                        modifier = Modifier.testTag("interlink-import-review-destination"))
                    if (review.source.canRemember) Row(Modifier.fillMaxWidth().heightIn(min = 48.dp).testTag("interlink-import-remember")
                        .toggleable(value = rememberSource, enabled = !busy && !submitted && review.source.canRemember, role = Role.Checkbox, onValueChange = { rememberSource = it }),
                        verticalAlignment = Alignment.CenterVertically) {
                        Checkbox(checked = rememberSource, onCheckedChange = null, enabled = !busy && !submitted && review.source.canRemember)
                        Column(Modifier.weight(1f).padding(start = 8.dp)) {
                            Text(stringResource(R.string.interlink_import_remember))
                            Text(stringResource(if (review.source.source.camera)
                                R.string.interlink_import_remember_camera else R.string.interlink_import_remember_detail), style = MaterialTheme.typography.bodySmall)
                        }
                    }
                    else Column(Modifier.testTag("interlink-import-connection-only"), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                        Text(stringResource(R.string.interlink_import_connection_only), style = MaterialTheme.typography.titleSmall)
                        Text(stringResource(R.string.interlink_import_unstable_camera), style = MaterialTheme.typography.bodySmall)
                    }
                    Text(stringResource(R.string.interlink_import_originals), style = MaterialTheme.typography.bodySmall)
                }
            }
        },
        confirmButton = {
            Button(enabled = !busy && !submitted && (review.pause || (review.source.confirmation != null && review.context.spaceAfterReserve != null)),
                modifier = Modifier.testTag("interlink-import-confirm"), onClick = {
                    if (!submitted) {
                        submitted = true
                        confirm(if (review.pause) PiImportMode.OFF else if (rememberSource && review.source.canRemember) PiImportMode.REMEMBER else PiImportMode.ONCE)
                    }
                }) { Text(stringResource(if (review.pause) R.string.interlink_import_pause else if (rememberSource)
                    R.string.interlink_import_remember_action else R.string.interlink_import_once)) }
        },
        dismissButton = { TextButton(enabled = !busy && !submitted, onClick = dismiss) { Text(stringResource(R.string.action_cancel)) } },
    )
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
internal fun InterlinkImportRequests(
    dashboard: ApplianceDashboardState, imports: PiImportsState, busy: Boolean,
    check: (PiSavedImport) -> Unit, withdraw: (PiSavedImport) -> Unit,
) {
    val selected = dashboard.selection ?: return
    if (selected != imports.selected || imports.actions.isEmpty()) return
    val connected = dashboard.connection == ApplianceConnectionState.CONNECTED
    val shown = imports.actions.filter { !it.resolved || it in imports.actions.take(5) }
    Card(Modifier.fillMaxWidth().testTag("interlink-import-requests")) {
        Column(Modifier.padding(20.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Text(stringResource(R.string.interlink_import_requests_title), style = MaterialTheme.typography.titleLarge,
                modifier = Modifier.semantics { heading() })
            if (!connected) Text(stringResource(R.string.interlink_import_history_offline))
            shown.forEachIndexed { index, action ->
                if (index > 0) HorizontalDivider()
                val previous = action.choice.bond.nativeGeneration != selected.confirmedGeneration ||
                    imports.observation?.context?.bond?.let { it != action.choice.bond } == true
                val (title, detail) = when {
                    action.supersededBy != null -> R.string.interlink_import_superseded to R.string.interlink_import_superseded_detail
                    previous && !action.resolved -> R.string.interlink_import_previous to R.string.interlink_import_previous_detail
                    action.outcome == PiImportOutcome.WITHDRAWN -> R.string.interlink_import_withdrawn to R.string.interlink_import_withdrawn_detail
                    action.outcome == PiImportOutcome.EXPIRED -> R.string.interlink_import_expired to R.string.interlink_import_expired_detail
                    action.outcome == PiImportOutcome.ACCEPTED && action.choice.mode == PiImportMode.OFF -> R.string.interlink_import_paused to R.string.interlink_import_pause_detail
                    action.outcome == PiImportOutcome.ACCEPTED -> R.string.interlink_import_accepted to R.string.interlink_import_accepted_detail
                    action.withdrawalRequested -> R.string.interlink_import_withdrawing to R.string.interlink_import_withdrawing_detail
                    action.outcome == PiImportOutcome.UNCERTAIN -> R.string.interlink_import_uncertain to R.string.interlink_import_uncertain_detail
                    else -> R.string.interlink_import_pending to R.string.interlink_import_pending_detail
                }
                Column(Modifier.fillMaxWidth().testTag("interlink-import-request-${action.choice.id}"), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text(action.source.source.name, style = MaterialTheme.typography.titleMedium)
                    Text(stringResource(title), style = MaterialTheme.typography.labelLarge,
                        modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite })
                    Text(stringResource(detail), style = MaterialTheme.typography.bodySmall)
                    if (!action.resolved && !previous) {
                        FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                            OutlinedButton(enabled = !busy && connected && imports.actionsAvailable,
                                onClick = { check(action) }, modifier = Modifier.testTag("interlink-import-check-${action.choice.id}")) {
                                Text(stringResource(R.string.interlink_import_check))
                            }
                            if (!action.withdrawalRequested) TextButton(enabled = !busy && connected && imports.actionsAvailable,
                                onClick = { withdraw(action) }, modifier = Modifier.testTag("interlink-import-withdraw-${action.choice.id}")) {
                                Text(stringResource(R.string.interlink_import_withdraw))
                            }
                        }
                    }
                }
            }
        }
    }
}
