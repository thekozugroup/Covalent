package life.michaelwong.covalent

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import life.michaelwong.covalent.appliance.*

@OptIn(ExperimentalLayoutApi::class)
@Composable
internal fun InterlinkUpdatesCard(
    dashboard: ApplianceDashboardState, updates: PiUpdatesState, busy: Boolean,
    request: (Boolean) -> Unit, check: (PiSavedUpdate) -> Unit,
) {
    val selected = dashboard.selection ?: return
    val connected = dashboard.connection == ApplianceConnectionState.CONNECTED
    val context = updates.observation?.context?.takeIf {
        connected && updates.selected == selected && it.bond.appliance == selected.appliance &&
            it.bond.controller == selected.local && it.bond.nativeGeneration == dashboard.observation?.bondGeneration
    }
    val canChange = context?.configured == true && !context.busy && updates.actionsAvailable && !busy
    val latest = updates.actions.filter { it.choice.bond.appliance == selected.appliance && it.choice.bond.controller == selected.local }
        .maxByOrNull { it.order }
    Card(Modifier.fillMaxWidth().testTag("interlink-updates-card")) {
        Column(Modifier.padding(20.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Text(stringResource(R.string.interlink_updates_title), style = MaterialTheme.typography.titleLarge,
                modifier = Modifier.semantics { heading() })
            if (context?.configured == true) {
                Row(Modifier.fillMaxWidth().heightIn(min = 56.dp).testTag("interlink-automatic-updates")
                    .toggleable(value = context.enabled, enabled = canChange, role = Role.Switch, onValueChange = request),
                    verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(16.dp)) {
                    Text(stringResource(R.string.interlink_updates_toggle), style = MaterialTheme.typography.titleMedium,
                        modifier = Modifier.weight(1f))
                    Switch(checked = context.enabled, onCheckedChange = null, enabled = canChange)
                }
            }
            Text(stringResource(when {
                dashboard.connection == ApplianceConnectionState.CHECKING -> R.string.interlink_updates_checking
                dashboard.connection == ApplianceConnectionState.APPROVAL_REQUIRED -> R.string.interlink_updates_approve_phone
                !connected -> R.string.interlink_updates_reconnect
                !updates.actionsAvailable -> R.string.interlink_updates_history_unavailable
                updates.observation?.supported == false -> R.string.interlink_updates_control_unsupported
                context == null -> R.string.interlink_updates_unavailable
                !context.configured -> R.string.interlink_updates_unconfigured
                context.busy -> R.string.interlink_updates_busy
                context.enabled -> R.string.interlink_updates_automatic
                else -> R.string.interlink_updates_paused
            }))
            if (latest != null && !latest.resolved) {
                HorizontalDivider()
                Text(stringResource(R.string.interlink_updates_unconfirmed), style = MaterialTheme.typography.titleSmall)
                Text(stringResource(if (latest.choice.enabled) R.string.interlink_updates_requested_on else R.string.interlink_updates_requested_off))
                Text(stringResource(R.string.interlink_updates_pending_detail), style = MaterialTheme.typography.bodySmall)
                FlowRow(horizontalArrangement = Arrangement.spacedBy(12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    OutlinedButton(enabled = !busy && connected && updates.actionsAvailable, onClick = { check(latest) },
                        modifier = Modifier.testTag("interlink-updates-check")) { Text(stringResource(R.string.interlink_updates_check)) }
                    if (context?.configured == true) TextButton(enabled = canChange, onClick = { request(context.enabled) },
                        modifier = Modifier.testTag("interlink-updates-keep")) { Text(stringResource(R.string.interlink_updates_keep)) }
                }
            } else if (latest?.observedContext != null && latest.receipt?.outcome != PiUpdateOutcome.APPLIED) {
                Text(stringResource(R.string.interlink_updates_observed), style = MaterialTheme.typography.bodySmall)
            }
        }
    }
}

@Composable
internal fun InterlinkUpdateConfirmation(review: PiUpdateReview, busy: Boolean, dismiss: () -> Unit, confirm: () -> Unit) {
    AlertDialog(
        onDismissRequest = { if (!busy) dismiss() },
        title = { Text(stringResource(if (review.choice.enabled) R.string.interlink_updates_enable_title else R.string.interlink_updates_pause_title)) },
        text = { Column(Modifier.verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Text(review.selected.name, style = MaterialTheme.typography.titleMedium)
            Text(stringResource(if (review.choice.enabled) R.string.interlink_updates_enable_detail else R.string.interlink_updates_pause_detail))
            Text(stringResource(R.string.interlink_updates_confirmation_detail), style = MaterialTheme.typography.bodySmall)
        } },
        confirmButton = { Button(enabled = !busy, onClick = { if (!busy) confirm() }, modifier = Modifier.testTag("interlink-updates-confirm")) {
            Text(stringResource(R.string.interlink_updates_confirm))
        } },
        dismissButton = { TextButton(enabled = !busy, onClick = dismiss) { Text(stringResource(R.string.action_cancel)) } },
    )
}
