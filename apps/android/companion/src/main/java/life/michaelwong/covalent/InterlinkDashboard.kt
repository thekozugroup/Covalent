package life.michaelwong.covalent

import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.repeatOnLifecycle
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import life.michaelwong.covalent.appliance.*
import life.michaelwong.covalent.data.CovalentNodeClient
import life.michaelwong.covalent.node.EmbeddedNodeManager

@Composable
internal fun InterlinkApplianceDashboard(manager: EmbeddedNodeManager) {
    val context = LocalContext.current
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    val store = remember(context) { ApplianceSelectionStore(context.applicationContext) }
    val controller = remember(manager, store) {
        val client = CovalentNodeClient()
        fun connection() = checkNotNull(manager.localConnectionForFolderSync())
        ApplianceDashboardController(store, object : ApplianceDashboardApi {
            override fun context() = connection().let { client.applianceContext(it.baseUrl, it.token) }
            override fun names() = connection().let { client.peerGrants(it.baseUrl, it.token) }
                .filterNot { it.revoked }.associate { it.peerDeviceId to it.displayName }
            override fun status(peerId: String, localId: String) = connection().let {
                client.applianceStatus(it.baseUrl, it.token, peerId, localId)
            }
        })
    }
    val scope = rememberCoroutineScope()
    val gate = remember { Mutex() }
    var state by remember { mutableStateOf(ApplianceDashboardState()) }
    var busy by remember { mutableStateOf(false) }
    var refresh by remember { mutableIntStateOf(0) }
    var choosing by remember { mutableStateOf(false) }
    var confirmation by remember { mutableStateOf<ApplianceCandidate?>(null) }
    var selectionFailed by remember { mutableStateOf(false) }

    LaunchedEffect(manager, lifecycle, refresh) {
        lifecycle.repeatOnLifecycle(Lifecycle.State.STARTED) {
            while (true) {
                gate.withLock {
                    busy = true
                    state = state.copy(connection = ApplianceConnectionState.CHECKING, observation = null)
                    try { state = withContext(Dispatchers.IO) { controller.refresh(manager.folderSyncRequested()) } }
                    finally { busy = false }
                }
                delay(15_000)
            }
        }
    }

    InterlinkApplianceCard(state, busy, { refresh++ }, { choosing = true })
    if (selectionFailed) Text(stringResource(R.string.interlink_pi_selection_failed), color = MaterialTheme.colorScheme.error)
    if (choosing) AlertDialog(
        onDismissRequest = { choosing = false },
        title = { Text(stringResource(R.string.interlink_pi_choose)) },
        text = {
            androidx.compose.foundation.lazy.LazyColumn(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                items(state.candidates.size) { index ->
                    val candidate = state.candidates[index]
                    OutlinedButton(onClick = { choosing = false; confirmation = candidate }, enabled = !busy,
                        modifier = Modifier.fillMaxWidth()) {
                        Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                            Text(candidate.name)
                            Text(candidate.identity.fingerprint, style = MaterialTheme.typography.labelSmall)
                        }
                    }
                }
            }
        },
        confirmButton = { TextButton(onClick = { choosing = false }) { Text(stringResource(R.string.action_cancel)) } },
    )
    confirmation?.let { candidate ->
        AlertDialog(
            onDismissRequest = { if (!busy) confirmation = null },
            title = { Text(stringResource(R.string.interlink_pi_use, candidate.name)) },
            text = {
                Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                    Text(stringResource(R.string.interlink_pi_choose_detail))
                    Text(candidate.identity.fingerprint, style = MaterialTheme.typography.labelLarge)
                }
            },
            confirmButton = {
                Button(enabled = !busy, onClick = {
                    scope.launch {
                        gate.withLock {
                            busy = true; selectionFailed = false
                            val expected = state.selection
                            val shownContext = state.context
                            try {
                                checkNotNull(shownContext)
                                withContext(Dispatchers.IO) { controller.select(expected, shownContext, candidate) }
                                confirmation = null
                                refresh++
                            } catch (cancelled: kotlinx.coroutines.CancellationException) { throw cancelled }
                            catch (_: Exception) { selectionFailed = true; confirmation = null }
                            finally { busy = false }
                        }
                    }
                }) { Text(stringResource(R.string.interlink_pi_remember)) }
            },
            dismissButton = { TextButton(enabled = !busy, onClick = { confirmation = null }) { Text(stringResource(R.string.action_cancel)) } },
        )
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
internal fun InterlinkApplianceCard(
    state: ApplianceDashboardState,
    busy: Boolean,
    refresh: () -> Unit,
    choose: () -> Unit,
) {
    Card(Modifier.fillMaxWidth().testTag("interlink-pi-card"),
        colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.secondaryContainer)) {
        Column(Modifier.padding(20.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Text(stringResource(R.string.interlink_pi_heading), style = MaterialTheme.typography.labelLarge)
            Text(state.selection?.name ?: stringResource(if (state.connection == ApplianceConnectionState.SAVED_CHOICE_UNAVAILABLE)
                R.string.interlink_pi_saved_title else R.string.interlink_pi_no_selection_title),
                style = MaterialTheme.typography.headlineSmall, fontWeight = FontWeight.SemiBold,
                modifier = Modifier.semantics { heading() }.testTag("interlink-pi-name"))
            val connected = state.connection == ApplianceConnectionState.CONNECTED
            Text(stringResource(when (state.connection) {
                ApplianceConnectionState.NO_SELECTION -> R.string.interlink_pi_no_selection
                ApplianceConnectionState.CHECKING -> R.string.interlink_pi_checking
                ApplianceConnectionState.CONNECTED -> R.string.interlink_pi_connected
                ApplianceConnectionState.APPROVAL_REQUIRED -> R.string.interlink_pi_approval
                ApplianceConnectionState.UNAVAILABLE -> R.string.interlink_pi_unavailable
                ApplianceConnectionState.PHONE_UNAVAILABLE -> R.string.interlink_pi_phone_unavailable
                ApplianceConnectionState.IDENTITY_CHANGED -> R.string.interlink_pi_identity_changed
                ApplianceConnectionState.UNSUPPORTED -> R.string.interlink_pi_unsupported
                ApplianceConnectionState.SAVED_CHOICE_UNAVAILABLE -> R.string.interlink_pi_saved_unavailable
            }), style = MaterialTheme.typography.titleMedium,
                modifier = Modifier.testTag("interlink-pi-state").semantics { liveRegion = LiveRegionMode.Polite })
            Text(stringResource(when {
                state.connection == ApplianceConnectionState.SAVED_CHOICE_UNAVAILABLE -> R.string.interlink_pi_saved_detail
                state.connection == ApplianceConnectionState.IDENTITY_CHANGED -> R.string.interlink_pi_identity_detail
                state.connection == ApplianceConnectionState.APPROVAL_REQUIRED -> R.string.interlink_pi_approval_detail
                state.connection == ApplianceConnectionState.UNSUPPORTED -> R.string.interlink_pi_unsupported_detail
                state.selection == null -> R.string.interlink_pi_pair_detail
                connected -> R.string.interlink_pi_connected_detail
                else -> R.string.interlink_pi_retained_detail
            }))
            if (state.connection == ApplianceConnectionState.APPROVAL_REQUIRED) {
                state.selection?.let { selection ->
                    Text(stringResource(R.string.interlink_pi_phone_fingerprint, selection.local.fingerprint),
                        style = MaterialTheme.typography.bodySmall, modifier = Modifier.testTag("interlink-phone-fingerprint"))
                    Text(stringResource(R.string.interlink_pi_host_fingerprint, selection.appliance.fingerprint),
                        style = MaterialTheme.typography.bodySmall, modifier = Modifier.testTag("interlink-host-fingerprint"))
                }
            }
            if (connected) {
                state.observation?.let { observation ->
                    HorizontalDivider()
                    Text(stringResource(when (observation.syncState) {
                        ApplianceSyncState.UNAVAILABLE -> R.string.interlink_pi_sync_unavailable
                        ApplianceSyncState.STOPPED -> R.string.interlink_pi_sync_stopped
                        ApplianceSyncState.SCANNING -> R.string.interlink_pi_sync_scanning
                        ApplianceSyncState.RUNNING -> R.string.interlink_pi_sync_running
                        ApplianceSyncState.STOPPING -> R.string.interlink_pi_sync_stopping
                        ApplianceSyncState.NEEDS_ATTENTION -> R.string.interlink_pi_sync_attention
                    }))
                    if (!observation.healthFresh && observation.syncState != ApplianceSyncState.UNAVAILABLE) {
                        Text(stringResource(R.string.interlink_pi_health_unknown), style = MaterialTheme.typography.bodySmall)
                    }
                    Text(stringResource(R.string.interlink_pi_cloud_unknown), style = MaterialTheme.typography.bodySmall)
                }
            }
            FlowRow(horizontalArrangement = Arrangement.spacedBy(12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                OutlinedButton(onClick = refresh, enabled = !busy, modifier = Modifier.testTag("interlink-pi-refresh")) {
                    Text(stringResource(R.string.action_refresh))
                }
                if (state.candidates.isNotEmpty() && state.connection != ApplianceConnectionState.SAVED_CHOICE_UNAVAILABLE) {
                    if (state.selection == null) {
                        Button(onClick = choose, enabled = !busy, modifier = Modifier.testTag("interlink-pi-choose")) {
                            Text(stringResource(R.string.interlink_pi_choose))
                        }
                    } else {
                        TextButton(onClick = choose, enabled = !busy, modifier = Modifier.testTag("interlink-pi-choose")) {
                            Text(stringResource(R.string.interlink_pi_change))
                        }
                    }
                }
            }
        }
    }
}
