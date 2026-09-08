package life.michaelwong.covalent.ui

import android.Manifest
import android.content.pm.PackageManager
import android.os.Build
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.Button
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Card
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import java.util.UUID
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.isActive
import kotlinx.coroutines.withContext
import life.michaelwong.covalent.R
import life.michaelwong.covalent.data.CovalentNodeClient
import life.michaelwong.covalent.model.FolderShare
import life.michaelwong.covalent.model.FolderSharePhase
import life.michaelwong.covalent.model.FolderSyncStatus
import life.michaelwong.covalent.model.FolderSyncAvailability
import life.michaelwong.covalent.model.FolderSyncLifecycle
import life.michaelwong.covalent.model.FolderSyncIssue
import life.michaelwong.covalent.model.FolderShareSummary
import life.michaelwong.covalent.model.summaryFor
import life.michaelwong.covalent.model.NodeConnection
import life.michaelwong.covalent.node.EmbeddedNodeManager
import life.michaelwong.covalent.sync.FolderSyncActions
import life.michaelwong.covalent.sync.FolderSyncGrantStore
import life.michaelwong.covalent.sync.FolderSyncSpecialAccess
import life.michaelwong.covalent.sync.NodeFolderSyncApi
import life.michaelwong.covalent.sync.RawFolderAccess
import life.michaelwong.covalent.sync.RawFolderEntry

@Composable
internal fun FolderSyncScreen(manager: EmbeddedNodeManager, modifier: Modifier = Modifier) {
    val context = LocalContext.current
    val hostUnavailableMessage = stringResource(R.string.folder_sync_host_unavailable)
    val localNetworkDeclinedMessage = stringResource(R.string.folder_sync_local_network_declined)
    val accessDeclinedMessage = stringResource(R.string.folder_sync_access_declined)
    val pairedDeviceName = stringResource(R.string.folder_sync_paired_device)
    val scope = androidx.compose.runtime.rememberCoroutineScope()
    val grants = remember(context) { FolderSyncGrantStore(context.applicationContext) }
    val api = remember { NodeFolderSyncApi(CovalentNodeClient()) }
    val browser = remember(context) { RawFolderAccess(context.applicationContext) }
    var accessGranted by remember { mutableStateOf(FolderSyncSpecialAccess.granted()) }
    var hostRequested by remember { mutableStateOf(manager.folderSyncRequested()) }
    var status by remember { mutableStateOf<FolderSyncStatus?>(null) }
    var error by remember { mutableStateOf<String?>(null) }
    var currentFolder by remember { mutableStateOf<String?>(null) }
    var selectedFolder by remember { mutableStateOf<String?>(null) }
    var entries by remember { mutableStateOf<List<RawFolderEntry>>(emptyList()) }
    var label by remember { mutableStateOf("") }
    var selectedPeer by remember { mutableStateOf<String?>(null) }
    var busy by remember { mutableStateOf(false) }
    var pendingRepairOffers by remember { mutableStateOf<Set<String>>(emptySet()) }

    fun connection(): NodeConnection = manager.localConnectionForFolderSync()
        ?: throw IllegalStateException("The on-phone node is still starting.")

    fun refresh() {
        if (busy) return
        accessGranted = FolderSyncSpecialAccess.granted()
        busy = true
        error = null
        scope.launch {
            runCatching {
                val ready = withContext(Dispatchers.IO) {
                    repeat(32) {
                        manager.localConnectionForFolderSync()?.let { return@withContext it }
                        delay(250)
                    }
                    throw IllegalStateException("not ready")
                }
                withContext(Dispatchers.IO) {
                    val previousChoices = grants.records().mapNotNull { it.offerId }.toSet()
                    val snapshot = api.status(ready).also(grants::reconcile)
                    val retiredRecipientChoice = snapshot.shares.any { share ->
                        share.incoming && share.supersededOfferIds.any { it in previousChoices }
                    }
                    Triple(snapshot, grants.records()
                        .filter { it.pendingRoot != null && !it.pendingRemoval }
                        .mapNotNull { it.offerId }
                        .toSet(), retiredRecipientChoice)
                }
            }.onSuccess { (snapshot, pending, retiredRecipientChoice) ->
                status = snapshot
                pendingRepairOffers = pending
                if (retiredRecipientChoice) selectedFolder = null
            }.onFailure { error = folderSyncErrorText(it) }
            busy = false
        }
    }

    fun enableHostAndRefresh() {
        if (busy) return
        busy = true
        scope.launch {
            val enabled = withContext(Dispatchers.IO) { manager.enableFolderSyncHost() }
            hostRequested = enabled
            busy = false
            if (enabled) refresh()
            else error = hostUnavailableMessage
        }
    }

    fun actions() = FolderSyncActions(
        grants,
        api,
        ::connection,
        browser::select,
        manager::refreshFolderSyncAccess,
    )

    val localNetworkPermission = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission(),
    ) { granted ->
        if (granted) {
            if (hostRequested) {
                manager.refreshFolderSyncAccess()
                refresh()
            } else {
                enableHostAndRefresh()
            }
        } else {
            error = localNetworkDeclinedMessage
        }
    }
    val specialAccess = rememberLauncherForActivityResult(
        ActivityResultContracts.StartActivityForResult(),
    ) {
        accessGranted = FolderSyncSpecialAccess.granted()
        if (!accessGranted) {
            error = accessDeclinedMessage
        } else if (
            Build.VERSION.SDK_INT >= 37 && ContextCompat.checkSelfPermission(
                context,
                Manifest.permission.ACCESS_LOCAL_NETWORK,
            ) != PackageManager.PERMISSION_GRANTED
        ) {
            localNetworkPermission.launch(Manifest.permission.ACCESS_LOCAL_NETWORK)
        } else {
            if (hostRequested) {
                manager.refreshFolderSyncAccess()
                refresh()
            } else {
                enableHostAndRefresh()
            }
        }
    }

    LaunchedEffect(hostRequested) {
        while (hostRequested && isActive) {
            refresh()
            delay(3_000)
        }
    }

    LazyColumn(
        modifier.fillMaxSize().testTag("folder-sync-list"),
        contentPadding = PaddingValues(20.dp, 14.dp, 20.dp, 80.dp),
        verticalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        item {
            Text(
                stringResource(R.string.folder_sync_title),
                style = MaterialTheme.typography.headlineMedium,
                fontWeight = FontWeight.SemiBold,
            )
            Spacer(Modifier.height(6.dp))
            Text(stringResource(R.string.folder_sync_subtitle))
        }
        error?.let { message -> item { Text(message, color = MaterialTheme.colorScheme.error) } }
        if (!FolderSyncSpecialAccess.supported()) {
            item {
                Card(Modifier.fillMaxWidth()) {
                    Text(
                        stringResource(R.string.folder_sync_personal_build_only),
                        Modifier.padding(16.dp),
                    )
                }
            }
            return@LazyColumn
        }
        if (!accessGranted) {
            item {
                Card(Modifier.fillMaxWidth()) {
                    Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
                        Text(stringResource(R.string.folder_sync_access_title), fontWeight = FontWeight.SemiBold)
                        Text(stringResource(R.string.folder_sync_access_explanation))
                        Button(onClick = {
                            runCatching {
                                specialAccess.launch(FolderSyncSpecialAccess.settingsIntent(context))
                            }.onFailure { error = folderSyncErrorText(it) }
                        }) {
                            Text(stringResource(R.string.folder_sync_open_special_access))
                        }
                    }
                }
            }
        }
        if (!hostRequested) {
            if (accessGranted) {
                item {
                    Card(Modifier.fillMaxWidth()) {
                        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
                            Text(stringResource(R.string.folder_sync_access_title), fontWeight = FontWeight.SemiBold)
                            Text(stringResource(R.string.folder_sync_access_explanation))
                            Button(onClick = {
                                if (
                                    Build.VERSION.SDK_INT >= 37 && ContextCompat.checkSelfPermission(
                                        context,
                                        Manifest.permission.ACCESS_LOCAL_NETWORK,
                                    ) != PackageManager.PERMISSION_GRANTED
                                ) {
                                    localNetworkPermission.launch(Manifest.permission.ACCESS_LOCAL_NETWORK)
                                } else {
                                    enableHostAndRefresh()
                                }
                            }) {
                                Text(stringResource(R.string.folder_sync_enable))
                            }
                        }
                    }
                }
            }
            return@LazyColumn
        }
        item {
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
                Text(
                    status?.let {
                        when {
                            it.issue != null || it.availability == FolderSyncAvailability.NEEDS_ATTENTION ->
                                stringResource(R.string.folder_sync_connection_attention)
                            it.lifecycle == FolderSyncLifecycle.INITIAL_SCANNING ->
                                stringResource(R.string.folder_sync_checking_folders)
                            it.lifecycle == FolderSyncLifecycle.STILL_STOPPING ->
                                stringResource(R.string.folder_sync_stopping)
                            it.lifecycle == FolderSyncLifecycle.RUNNING ->
                                stringResource(R.string.folder_sync_running)
                            else -> stringResource(R.string.folder_sync_ready_to_share)
                        }
                    } ?: stringResource(R.string.folder_sync_starting),
                )
                OutlinedButton(onClick = ::refresh, enabled = !busy) {
                    Text(stringResource(R.string.action_refresh_backups))
                }
            }
        }
        if (accessGranted) {
            item(key = "phone-pairing") {
                FolderSyncPairing(manager = manager, onPeersChanged = ::refresh)
            }
            item {
                FolderChooser(
                    current = currentFolder,
                    entries = entries,
                    selected = selectedFolder,
                    busy = busy,
                    onOpen = { path ->
                        busy = true
                        scope.launch {
                            runCatching { withContext(Dispatchers.IO) { browser.children(path) } }
                                .onSuccess {
                                    currentFolder = path
                                    entries = it
                                }
                                .onFailure { error = folderSyncErrorText(it) }
                            busy = false
                        }
                    },
                    onRoot = {
                        busy = true
                        scope.launch {
                            runCatching { withContext(Dispatchers.IO) { browser.roots() } }
                                .onSuccess {
                                    entries = it
                                    currentFolder = null
                                }
                                .onFailure { error = folderSyncErrorText(it) }
                            busy = false
                        }
                    },
                    onChoose = { path ->
                        busy = true
                        scope.launch {
                            runCatching { withContext(Dispatchers.IO) { browser.select(path) } }
                                .onSuccess { selectedFolder = it }
                                .onFailure { error = folderSyncErrorText(it) }
                            busy = false
                        }
                    },
                )
            }
        }
        val snapshot = status
        if (snapshot != null) {
            if (accessGranted && snapshot.issue != FolderSyncIssue.FOLDER_ACCESS) {
                item {
                    OutlinedTextField(
                        value = label,
                        onValueChange = { if (it.length <= 256) label = it },
                        label = { Text(stringResource(R.string.folder_sync_label)) },
                        modifier = Modifier.fillMaxWidth(),
                    )
                }
            }
            if (
                snapshot.lifecycle == FolderSyncLifecycle.NEEDS_ATTENTION ||
                snapshot.lifecycle == FolderSyncLifecycle.STILL_STOPPING
            ) {
                item {
                    Button(
                        enabled = !busy,
                        onClick = {
                            busy = true
                            scope.launch {
                                runCatching {
                                    withContext(Dispatchers.IO) {
                                        actions().retry()
                                    }
                                }.onFailure { error = folderSyncErrorText(it) }
                                busy = false
                                refresh()
                            }
                        },
                    ) { Text(stringResource(R.string.action_retry)) }
                }
            }
            if (accessGranted && snapshot.issue != FolderSyncIssue.FOLDER_ACCESS) {
                if (snapshot.peers.isEmpty()) {
                    item { Text(stringResource(R.string.folder_sync_pair_first)) }
                } else {
                    item { Text(stringResource(R.string.folder_sync_choose_peer), fontWeight = FontWeight.SemiBold) }
                    items(snapshot.peers, key = { it.peerId }) { peer ->
                        OutlinedButton(onClick = { selectedPeer = peer.peerId }, Modifier.fillMaxWidth()) {
                            Text(if (selectedPeer == peer.peerId) "✓ ${peer.displayName}" else peer.displayName)
                        }
                    }
                    item {
                        Button(
                            enabled = !busy && selectedFolder != null && selectedPeer != null && label.isNotBlank(),
                            onClick = {
                                busy = true
                                scope.launch {
                                    runCatching {
                                        withContext(Dispatchers.IO) {
                                            actions().offer(
                                                checkNotNull(selectedPeer),
                                                UUID.randomUUID(),
                                                label.trim(),
                                                checkNotNull(selectedFolder),
                                            )
                                        }
                                    }.onFailure { error = folderSyncErrorText(it) }
                                    busy = false
                                    refresh()
                                }
                            },
                        ) { Text(stringResource(R.string.folder_sync_offer)) }
                    }
                }
            }
            items(snapshot.shares, key = FolderShare::offerId) { share ->
                FolderShareCard(
                    status = snapshot,
                    share = share,
                    peerName = snapshot.peers.firstOrNull { it.peerId == share.peerId }?.displayName
                        ?: pairedDeviceName,
                    hasFolder = selectedFolder != null,
                    accessGranted = accessGranted,
                    hasPendingRepair = share.offerId in pendingRepairOffers,
                    busy = busy,
                    mutate = { action ->
                        busy = true
                        scope.launch {
                            runCatching {
                                withContext(Dispatchers.IO) {
                                    val actions = actions()
                                    when (action) {
                                        "accept" -> actions.accept(share.offerId, checkNotNull(selectedFolder))
                                        "pause" -> actions.pause(share.offerId, true)
                                        "resume" -> actions.pause(share.offerId, false)
                                        "renew" -> actions.renew(share.offerId)
                                        "repair" -> actions.repair(share.offerId, checkNotNull(selectedFolder))
                                        "retry-repair" -> actions.retryRepair(share.offerId)
                                        "remove" -> actions.remove(share.offerId)
                                        else -> throw IllegalArgumentException("unknown action")
                                    }
                                }
                            }.onFailure { error = folderSyncErrorText(it) }
                            busy = false
                            refresh()
                        }
                    },
                )
            }
        }
    }
}

@Composable
private fun FolderChooser(
    current: String?,
    entries: List<RawFolderEntry>,
    selected: String?,
    busy: Boolean,
    onOpen: (String) -> Unit,
    onRoot: () -> Unit,
    onChoose: (String) -> Unit,
) {
    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(stringResource(R.string.folder_sync_choose_folder), fontWeight = FontWeight.SemiBold)
            Text(selected ?: stringResource(R.string.folder_sync_no_folder))
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                OutlinedButton(onClick = onRoot, enabled = !busy) {
                    Text(stringResource(R.string.folder_sync_storage_roots))
                }
                current?.let { path ->
                    Button(onClick = { onChoose(path) }, enabled = !busy) {
                        Text(stringResource(R.string.folder_sync_use_folder))
                    }
                }
            }
            entries.forEach { entry ->
                OutlinedButton(
                    onClick = { onOpen(entry.absolutePath) },
                    modifier = Modifier.fillMaxWidth(),
                    enabled = !busy,
                ) {
                    Text(entry.name.ifBlank { entry.absolutePath })
                }
            }
        }
    }
}

@Composable
private fun FolderShareCard(
    status: FolderSyncStatus,
    share: FolderShare,
    peerName: String,
    hasFolder: Boolean,
    accessGranted: Boolean,
    hasPendingRepair: Boolean,
    busy: Boolean,
    mutate: (String) -> Unit,
) {
    var showingRemovalConfirmation by remember(share.offerId) { mutableStateOf(false) }
    if (showingRemovalConfirmation) {
        AlertDialog(
            onDismissRequest = { showingRemovalConfirmation = false },
            title = { Text(stringResource(R.string.folder_sync_stop_title, share.label)) },
            text = { Text(stringResource(R.string.folder_sync_stop_detail, peerName)) },
            confirmButton = {
                TextButton(
                    onClick = {
                        showingRemovalConfirmation = false
                        mutate("remove")
                    },
                    enabled = !busy,
                ) { Text(stringResource(R.string.folder_sync_remove)) }
            },
            dismissButton = {
                TextButton(onClick = { showingRemovalConfirmation = false }) {
                    Text(stringResource(R.string.folder_sync_keep_sharing))
                }
            },
        )
    }
    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(share.label, fontWeight = FontWeight.SemiBold)
            val summary = when (status.summaryFor(share)) {
                FolderShareSummary.INVITATION_EXPIRED -> stringResource(R.string.folder_sync_invitation_expired)
                FolderShareSummary.PAUSED -> stringResource(R.string.folder_sync_connection_paused)
                FolderShareSummary.CHECKING -> stringResource(R.string.folder_sync_connection_checking)
                FolderShareSummary.NEEDS_ATTENTION -> stringResource(R.string.folder_sync_connection_attention)
                FolderShareSummary.WAITING_FOR_OTHER_DEVICE -> stringResource(R.string.folder_sync_waiting_other_device)
                FolderShareSummary.OFFLINE -> stringResource(R.string.folder_sync_connection_offline)
                FolderShareSummary.SYNCING -> stringResource(R.string.folder_sync_connection_syncing)
                FolderShareSummary.WAITING_FOR_PEER -> stringResource(R.string.folder_sync_waiting_for_device, peerName)
                FolderShareSummary.CONNECTED -> stringResource(R.string.folder_sync_connected_to_device, peerName)
                FolderShareSummary.CONNECTION_UNKNOWN -> stringResource(R.string.folder_sync_connection_unknown)
            }
            Text(summary)
            if (share.expired) Text(stringResource(
                if (share.incoming) R.string.folder_sync_renew_incoming_detail else R.string.folder_sync_renew_detail,
            ), color = MaterialTheme.colorScheme.onSurfaceVariant)
            if (status.issue == FolderSyncIssue.FOLDER_ACCESS && share.phase != FolderSharePhase.REMOVED) {
                Text(
                    stringResource(
                        if (accessGranted) R.string.folder_sync_choose_again_detail
                        else R.string.folder_sync_restore_access_detail,
                    ),
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                if (accessGranted) {
                    Button(
                        onClick = { mutate(if (hasPendingRepair) "retry-repair" else "repair") },
                        enabled = !busy && (hasPendingRepair || hasFolder),
                    ) {
                        Text(
                            stringResource(
                                if (hasPendingRepair) R.string.folder_sync_retry_saved_repair
                                else R.string.folder_sync_use_selected_again,
                            ),
                        )
                    }
                }
            }
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                if (status.issue != FolderSyncIssue.FOLDER_ACCESS && !share.incoming && share.expired &&
                    share.phase in setOf(FolderSharePhase.OFFERED, FolderSharePhase.PAUSED)
                ) {
                    Button(onClick = { mutate("renew") }, enabled = !busy) {
                        Text(stringResource(R.string.folder_sync_renew))
                    }
                }
                if (
                    status.issue != FolderSyncIssue.FOLDER_ACCESS &&
                    share.incoming && share.phase == FolderSharePhase.OFFERED
                ) {
                    Button(onClick = { mutate("accept") }, enabled = hasFolder && !share.expired && !busy) {
                        Text(stringResource(R.string.action_accept_invitation))
                    }
                }
                if (status.issue != FolderSyncIssue.FOLDER_ACCESS && share.phase == FolderSharePhase.READY) {
                    OutlinedButton(onClick = { mutate("pause") }, enabled = !busy) { Text(stringResource(R.string.action_pause)) }
                }
                if (status.issue != FolderSyncIssue.FOLDER_ACCESS && share.phase == FolderSharePhase.PAUSED && !share.expired) {
                    OutlinedButton(onClick = { mutate("resume") }, enabled = !busy) { Text(stringResource(R.string.action_resume)) }
                }
                if (share.phase != FolderSharePhase.REMOVED) {
                    OutlinedButton(onClick = { showingRemovalConfirmation = true }, enabled = !busy) {
                        Text(stringResource(R.string.folder_sync_remove))
                    }
                }
            }
        }
    }
}

private fun folderSyncErrorText(error: Throwable): String = when (error) {
    is SecurityException -> "Android denied access to that folder."
    is IllegalArgumentException -> "That folder or request is not valid for sync."
    else -> "Folder sync needs attention. Refresh to check the latest status."
}
