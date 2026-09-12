package life.michaelwong.covalent.ui

import android.Manifest
import android.content.pm.PackageManager
import android.os.Build
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
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
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.Alignment
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
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
import life.michaelwong.covalent.data.NodeApiException
import life.michaelwong.covalent.model.FolderShare
import life.michaelwong.covalent.model.FolderLinkPolicy
import life.michaelwong.covalent.model.FolderLinkSettings
import life.michaelwong.covalent.model.FolderLinkSettingsState
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
import life.michaelwong.covalent.sync.FolderLinkSettingsChangeStore
import life.michaelwong.covalent.sync.SavedFolderLinkSettingsChange
import life.michaelwong.covalent.sync.FolderSyncSpecialAccess
import life.michaelwong.covalent.sync.NodeFolderSyncApi
import life.michaelwong.covalent.sync.PeerAddressUpdateDraft
import life.michaelwong.covalent.sync.PeerAddressUpdateOutcome
import life.michaelwong.covalent.sync.RawFolderAccess
import life.michaelwong.covalent.sync.RawFolderEntry
import life.michaelwong.covalent.sync.completePeerAddressUpdate

private data class LoadedFolderSyncStatus(
    val status: FolderSyncStatus,
    val pendingRepairOffers: Set<String>,
    val retiredFolderChoice: Boolean,
    val savedSettingsChanges: List<SavedFolderLinkSettingsChange>,
)

private data class LinkSettingsEditor(
    val folderId: String,
    val label: String,
    val current: FolderLinkSettingsState,
    val propagateSourceDeletions: Boolean,
    val restoreLocalDeletions: Boolean,
)

@Composable
internal fun FolderSyncScreen(
    manager: EmbeddedNodeManager,
    modifier: Modifier = Modifier,
    onProviderConnectionsChanged: suspend () -> Unit = {},
) {
    val context = LocalContext.current
    val hostUnavailableMessage = stringResource(R.string.folder_sync_host_unavailable)
    val localNetworkDeclinedMessage = stringResource(R.string.folder_sync_local_network_declined)
    val accessDeclinedMessage = stringResource(R.string.folder_sync_access_declined)
    val pairedDeviceName = stringResource(R.string.folder_sync_paired_device)
    val invalidPeerAddressMessage = stringResource(R.string.node_error_invalid_peer_address)
    val peerAddressChangedMessage = stringResource(R.string.node_error_peer_address_changed)
    val addressUpdatedMessage = stringResource(R.string.folder_sync_address_updated_checking)
    val addressSavedRefreshNeededMessage = stringResource(R.string.folder_sync_address_saved_refresh_needed)
    val settingsPendingMessage = stringResource(R.string.folder_link_settings_pending_error)
    val settingsConflictMessage = stringResource(R.string.folder_link_settings_conflict_error)
    val settingsUnknownMessage = stringResource(R.string.folder_link_settings_status_unknown)
    val scope = androidx.compose.runtime.rememberCoroutineScope()
    val grants = remember(context) { FolderSyncGrantStore(context.applicationContext) }
    val settingsChanges = remember(context) { FolderLinkSettingsChangeStore(context.applicationContext) }
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
    var propagateSourceDeletions by remember { mutableStateOf(false) }
    var restoreLocalDeletions by remember { mutableStateOf(false) }
    var destinationSource by remember { mutableStateOf<FolderShare?>(null) }
    var busy by remember { mutableStateOf(false) }
    var pendingRepairOffers by remember { mutableStateOf<Set<String>>(emptySet()) }
    var savedSettingsChanges by remember { mutableStateOf<List<SavedFolderLinkSettingsChange>>(emptyList()) }
    var settingsEditor by remember { mutableStateOf<LinkSettingsEditor?>(null) }
    var settingsConfirmation by remember { mutableStateOf<LinkSettingsEditor?>(null) }
    var addressEditor by remember { mutableStateOf<PeerAddressUpdateDraft?>(null) }
    var addressEditorError by remember { mutableStateOf<String?>(null) }
    var addressUpdateNotice by remember { mutableStateOf<String?>(null) }

    fun connection(): NodeConnection = manager.localConnectionForFolderSync()
        ?: throw IllegalStateException("The on-phone node is still starting.")

    suspend fun loadStatus(): LoadedFolderSyncStatus {
        val ready = withContext(Dispatchers.IO) {
            repeat(32) {
                manager.localConnectionForFolderSync()?.let { return@withContext it }
                delay(250)
            }
            throw IllegalStateException("not ready")
        }
        return withContext(Dispatchers.IO) {
            val previousChoices = grants.records().mapNotNull { it.offerId }.toSet()
            val snapshot = api.status(ready).also {
                grants.reconcile(it)
                settingsChanges.reconcile(it)
            }
            val retiredChoice = snapshot.shares.any { share ->
                if (share.phase == FolderSharePhase.REMOVED) {
                    share.offerId in previousChoices || share.supersededOfferIds.any { it in previousChoices }
                } else share.incoming && share.supersededOfferIds.any { it in previousChoices }
            }
            LoadedFolderSyncStatus(
                snapshot,
                grants.records()
                    .filter { it.pendingRoot != null && !it.pendingRemoval }
                    .mapNotNull { it.offerId }
                    .toSet(),
                retiredChoice,
                settingsChanges.records(),
            )
        }
    }

    fun applyStatus(loaded: LoadedFolderSyncStatus) {
        status = loaded.status
        pendingRepairOffers = loaded.pendingRepairOffers
        savedSettingsChanges = loaded.savedSettingsChanges
        if (loaded.retiredFolderChoice) selectedFolder = null
        if (
            addressUpdateNotice == addressSavedRefreshNeededMessage ||
            addressUpdateNotice == addressUpdatedMessage &&
            loaded.status.lifecycle != FolderSyncLifecycle.INITIAL_SCANNING
        ) {
            addressUpdateNotice = null
        }
    }

    fun refresh() {
        if (busy) return
        accessGranted = FolderSyncSpecialAccess.granted()
        busy = true
        error = null
        scope.launch {
            runCatching { loadStatus() }
                .onSuccess { applyStatus(it) }
                .onFailure { error = folderSyncErrorText(it) }
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

    fun submitSettings(
        folderId: String,
        expectedRevision: Long,
        settings: FolderLinkSettings,
        retry: SavedFolderLinkSettingsChange? = null,
    ) {
        if (busy) return
        busy = true
        error = null
        settingsEditor = null
        settingsConfirmation = null
        scope.launch {
            val prepared = runCatching {
                withContext(Dispatchers.IO) {
                    retry ?: settingsChanges.prepare(folderId, expectedRevision, settings)
                }
            }
            if (prepared.isFailure) {
                error = folderSyncErrorText(checkNotNull(prepared.exceptionOrNull()))
                busy = false
                return@launch
            }
            val change = checkNotNull(prepared.getOrNull())
            val persisted = runCatching { withContext(Dispatchers.IO) { settingsChanges.records() } }
            if (persisted.isFailure) {
                error = folderSyncErrorText(checkNotNull(persisted.exceptionOrNull()))
                busy = false
                return@launch
            }
            savedSettingsChanges = checkNotNull(persisted.getOrNull())
            val submission = runCatching {
                withContext(Dispatchers.IO) {
                    api.settings(
                        connection(),
                        change.folderId,
                        change.changeId,
                        change.expectedRevision,
                        change.settings,
                    )
                }
            }
            val submissionError = submission.exceptionOrNull()
            val code = (submissionError as? NodeApiException)?.code
            val storageFailure = if (code == "link_settings_pending" || code == "link_settings_conflict") {
                runCatching {
                    withContext(Dispatchers.IO) {
                        settingsChanges.markForReview(change.folderId, change.changeId)
                    }
                }
                    .exceptionOrNull()
            } else null
            val loaded = runCatching { loadStatus() }
            loaded.onSuccess(::applyStatus)
            val saved = runCatching { withContext(Dispatchers.IO) { settingsChanges.records() } }
            saved.onSuccess { savedSettingsChanges = it }
            error = when {
                storageFailure != null -> folderSyncErrorText(storageFailure)
                code == "link_settings_pending" -> settingsPendingMessage
                code == "link_settings_conflict" -> settingsConflictMessage
                submissionError != null -> settingsUnknownMessage
                loaded.isFailure || saved.isFailure -> settingsUnknownMessage
                else -> null
            }
            busy = false
        }
    }

    fun submitAddressUpdate() {
        if (busy) return
        val captured = runCatching { checkNotNull(addressEditor).capture() }
            .getOrElse {
                addressEditorError = invalidPeerAddressMessage
                return
            }
        addressEditor = captured
        addressEditorError = null
        addressUpdateNotice = null
        error = null
        busy = true
        scope.launch {
            when (val outcome = completePeerAddressUpdate(
                captured,
                submit = { request ->
                    withContext(Dispatchers.IO) { actions().refreshPeerAddress(request) }
                    Unit
                },
                reloadStatus = { loadStatus() },
                refreshProviders = onProviderConnectionsChanged,
                errorCode = { (it as? NodeApiException)?.code },
            )) {
                is PeerAddressUpdateOutcome.Updated -> {
                    addressEditor = null
                    addressEditorError = null
                    if (outcome.freshStatus == null) {
                        status = null
                        pendingRepairOffers = emptySet()
                        addressUpdateNotice = addressSavedRefreshNeededMessage
                        error = outcome.statusFailure?.let {
                            nodeFailureMessage(context, it, R.string.error_node_action_failed)
                        }
                    } else {
                        applyStatus(outcome.freshStatus)
                        addressUpdateNotice = addressUpdatedMessage
                        error = outcome.providerFailure?.let {
                            nodeFailureMessage(context, it, R.string.error_connection_failed)
                        }
                    }
                }
                is PeerAddressUpdateOutcome.Changed -> {
                    addressEditor = null
                    addressEditorError = null
                    status = null
                    pendingRepairOffers = emptySet()
                    outcome.freshStatus?.let { applyStatus(it) }
                    error = peerAddressChangedMessage
                }
                is PeerAddressUpdateOutcome.Failed -> {
                    addressEditor = outcome.draft
                    addressEditorError = nodeFailureMessage(
                        context,
                        outcome.failure,
                        R.string.error_node_action_failed,
                    )
                }
            }
            busy = false
        }
    }

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

    LaunchedEffect(status?.peers, addressEditor?.peerId) {
        val editor = addressEditor ?: return@LaunchedEffect
        val current = status?.peers?.firstOrNull { it.peerId == editor.peerId }
        if (editor.afterStatus(current?.peerId, current?.address) == null) {
            addressEditor = null
            addressEditorError = null
            error = peerAddressChangedMessage
        }
    }

    addressEditor?.let { editor ->
        AlertDialog(
            onDismissRequest = {
                if (!busy) {
                    addressEditor = null
                    addressEditorError = null
                }
            },
            title = { Text(stringResource(R.string.folder_sync_update_address_title, editor.displayName)) },
            text = {
                Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
                    Text(stringResource(R.string.folder_sync_update_address_detail))
                    Text(stringResource(R.string.folder_sync_saved_address, editor.savedAddress))
                    OutlinedTextField(
                        value = editor.candidateAddress,
                        onValueChange = { candidate ->
                            val updated = editor.edit(candidate)
                            if (updated == null) {
                                addressEditorError = invalidPeerAddressMessage
                            } else {
                                addressEditor = updated
                                addressEditorError = null
                            }
                        },
                        label = { Text(stringResource(R.string.folder_sync_new_address)) },
                        placeholder = { Text(stringResource(R.string.folder_sync_pair_address_example)) },
                        singleLine = true,
                        enabled = !busy,
                        keyboardOptions = KeyboardOptions(
                            capitalization = androidx.compose.ui.text.input.KeyboardCapitalization.None,
                            autoCorrectEnabled = false,
                            keyboardType = KeyboardType.Uri,
                            imeAction = ImeAction.Done,
                        ),
                        keyboardActions = KeyboardActions(onDone = { submitAddressUpdate() }),
                        modifier = Modifier.fillMaxWidth().testTag("folder-peer-address-input"),
                    )
                    addressEditorError?.let { Text(it, color = MaterialTheme.colorScheme.error) }
                }
            },
            confirmButton = {
                Button(
                    onClick = ::submitAddressUpdate,
                    enabled = !busy && editor.candidateAddress.isNotBlank(),
                    modifier = Modifier.testTag("folder-peer-address-confirm"),
                ) {
                    Text(stringResource(
                        if (editor.pendingRequest == null) R.string.folder_sync_verify_address
                        else R.string.folder_sync_retry_address_update,
                    ))
                }
            },
            dismissButton = {
                TextButton(
                    onClick = {
                        addressEditor = null
                        addressEditorError = null
                    },
                    enabled = !busy,
                ) { Text(stringResource(R.string.action_cancel)) }
            },
        )
    }

    settingsEditor?.let { editor ->
        LinkSettingsEditorDialog(
            editor = editor,
            busy = busy,
            onChange = { settingsEditor = it },
            onSave = {
                val enablesSourceDeletion =
                    !editor.current.settings.deletionPolicy.propagateSourceDeletions &&
                        editor.propagateSourceDeletions
                val enablesRestore =
                    !editor.current.settings.deletionPolicy.restoreLocalDeletions &&
                        editor.restoreLocalDeletions
                if (enablesSourceDeletion || enablesRestore) {
                    settingsEditor = null
                    settingsConfirmation = editor
                } else {
                    submitSettings(
                        editor.folderId,
                        editor.current.revision,
                        editor.proposedSettings(),
                    )
                }
            },
            onDismiss = { settingsEditor = null },
        )
    }
    settingsConfirmation?.let { editor ->
        LinkSettingsConfirmationDialog(
            editor = editor,
            busy = busy,
            onConfirm = {
                submitSettings(
                    editor.folderId,
                    editor.current.revision,
                    editor.proposedSettings(),
                )
            },
            onDismiss = {
                settingsConfirmation = null
                settingsEditor = editor
            },
        )
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
        addressUpdateNotice?.let { message -> item { Text(message) } }
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
                        Card(Modifier.fillMaxWidth()) {
                            Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                                OutlinedButton(
                                    onClick = { selectedPeer = peer.peerId },
                                    modifier = Modifier.fillMaxWidth(),
                                    enabled = !busy,
                                ) {
                                    Text(if (selectedPeer == peer.peerId) "✓ ${peer.displayName}" else peer.displayName)
                                }
                                peer.address?.let { address ->
                                    Text(stringResource(R.string.folder_sync_saved_address, address))
                                    TextButton(
                                        onClick = {
                                            addressEditor = PeerAddressUpdateDraft(
                                                peer.peerId,
                                                peer.displayName,
                                                address,
                                            )
                                            addressEditorError = null
                                            addressUpdateNotice = null
                                        },
                                        enabled = !busy,
                                        modifier = Modifier.testTag("folder-peer-address-${peer.peerId}"),
                                    ) { Text(stringResource(R.string.folder_sync_update_address)) }
                                }
                            }
                        }
                    }
                    item {
                        val peerName = snapshot.peers.firstOrNull { it.peerId == selectedPeer }?.displayName
                        if (selectedFolder != null && peerName != null) {
                            Card(Modifier.fillMaxWidth()) {
                                Column(
                                    Modifier.padding(16.dp),
                                    verticalArrangement = Arrangement.spacedBy(4.dp),
                                ) {
                                    Text(
                                        stringResource(R.string.folder_link_direction),
                                        fontWeight = FontWeight.SemiBold,
                                    )
                                    Text(stringResource(R.string.folder_link_source_folder, checkNotNull(selectedFolder)))
                                    Text(stringResource(R.string.folder_link_destination_peer, peerName))
                                }
                            }
                        }
                    }
                    item {
                        LinkPolicyControl(
                            checked = propagateSourceDeletions,
                            onCheckedChange = { propagateSourceDeletions = it },
                            title = stringResource(R.string.folder_link_source_deletions),
                            detail = stringResource(R.string.folder_link_source_deletions_detail),
                            tag = "folder-link-source-deletions",
                        )
                    }
                    item {
                        LinkPolicyControl(
                            checked = restoreLocalDeletions,
                            onCheckedChange = { restoreLocalDeletions = it },
                            title = stringResource(R.string.folder_link_destination_deletions),
                            detail = stringResource(R.string.folder_link_destination_deletions_detail),
                            tag = "folder-link-destination-deletions",
                        )
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
                                                FolderLinkPolicy(
                                                    propagateSourceDeletions = propagateSourceDeletions,
                                                    restoreLocalDeletions = restoreLocalDeletions,
                                                ),
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
            items(snapshot.shares.filter { it.phase != FolderSharePhase.REMOVED || it.remoteRemovalPending }, key = FolderShare::offerId) { share ->
                val settingsCardOfferId = snapshot.shares.firstOrNull {
                    it.folderId == share.folderId && it.linkPolicy != null &&
                        (it.phase != FolderSharePhase.REMOVED || it.remoteRemovalPending)
                }?.offerId
                FolderShareCard(
                    status = snapshot,
                    share = share,
                    peerName = snapshot.peers.firstOrNull { it.peerId == share.peerId }?.displayName
                        ?: pairedDeviceName,
                    hasFolder = selectedFolder != null,
                    accessGranted = accessGranted,
                    hasPendingRepair = share.offerId in pendingRepairOffers,
                    busy = busy,
                    addDestination = { destinationSource = share },
                    showLinkSettings = share.offerId == settingsCardOfferId,
                    savedSettingsChange = savedSettingsChanges.singleOrNull { it.folderId == share.folderId },
                    editSettings = { state, policy ->
                        settingsEditor = LinkSettingsEditor(
                            share.folderId,
                            share.label,
                            state,
                            policy.propagateSourceDeletions,
                            policy.restoreLocalDeletions,
                        )
                    },
                    retrySettings = { change ->
                        submitSettings(
                            change.folderId,
                            change.expectedRevision,
                            change.settings,
                            change,
                        )
                    },
                    reviewSettings = { state, policy ->
                        scope.launch {
                            runCatching {
                                withContext(Dispatchers.IO) { settingsChanges.takeForReview(share.folderId) }
                                withContext(Dispatchers.IO) { settingsChanges.records() }
                            }.onSuccess {
                                savedSettingsChanges = it
                                settingsEditor = LinkSettingsEditor(
                                    share.folderId,
                                    share.label,
                                    state,
                                    policy.propagateSourceDeletions,
                                    policy.restoreLocalDeletions,
                                )
                            }.onFailure { error = folderSyncErrorText(it) }
                        }
                    },
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
    destinationSource?.let { source ->
        val snapshot = status
        if (snapshot != null) {
            val existingDestinations = snapshot.shares
                .filter {
                    !it.incoming && it.folderId == source.folderId &&
                        (it.phase != FolderSharePhase.REMOVED || it.remoteRemovalPending)
                }
                .mapTo(mutableSetOf(), FolderShare::peerId)
            val availablePeers = snapshot.peers.filter { it.peerId !in existingDestinations }
            AlertDialog(
                onDismissRequest = { if (!busy) destinationSource = null },
                title = { Text(stringResource(R.string.folder_link_add_destination)) },
                text = {
                    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                        Text(source.label, fontWeight = FontWeight.SemiBold)
                        Text(stringResource(R.string.folder_link_source_this_phone))
                        Text(stringResource(R.string.folder_link_source_locked))
                        if (availablePeers.isEmpty()) {
                            Text(stringResource(R.string.folder_link_no_destinations))
                        } else {
                            availablePeers.forEach { peer ->
                                OutlinedButton(
                                    onClick = {
                                        busy = true
                                        scope.launch {
                                            runCatching {
                                                withContext(Dispatchers.IO) {
                                                    actions().addDestination(source, peer.peerId)
                                                }
                                            }.onSuccess {
                                                destinationSource = null
                                            }.onFailure { error = folderSyncErrorText(it) }
                                            busy = false
                                            refresh()
                                        }
                                    },
                                    enabled = !busy,
                                    modifier = Modifier.fillMaxWidth()
                                        .testTag("folder-link-add-${peer.peerId}"),
                                ) {
                                    Text(stringResource(R.string.folder_link_add_peer, peer.displayName))
                                }
                            }
                        }
                    }
                },
                confirmButton = {},
                dismissButton = {
                    TextButton(onClick = { destinationSource = null }, enabled = !busy) {
                        Text(stringResource(R.string.action_cancel))
                    }
                },
            )
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
    addDestination: () -> Unit,
    showLinkSettings: Boolean,
    savedSettingsChange: SavedFolderLinkSettingsChange?,
    editSettings: (FolderLinkSettingsState, FolderLinkPolicy) -> Unit,
    retrySettings: (SavedFolderLinkSettingsChange) -> Unit,
    reviewSettings: (FolderLinkSettingsState, FolderLinkPolicy) -> Unit,
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
            Text(
                if (share.incoming) stringResource(R.string.folder_link_source_peer, peerName)
                else stringResource(R.string.folder_link_source_this_phone),
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Text(
                if (share.incoming) stringResource(R.string.folder_link_destination_this_phone)
                else stringResource(R.string.folder_link_destination_peer, peerName),
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            share.linkPolicy?.let { policy ->
                Text(stringResource(R.string.folder_link_one_way), fontWeight = FontWeight.SemiBold)
                LinkPolicySummary(policy)
            } ?: Text(stringResource(R.string.folder_link_legacy_two_way), fontWeight = FontWeight.SemiBold)
            if (!share.incoming && share.linkPolicy != null && share.phase != FolderSharePhase.REMOVED) {
                OutlinedButton(
                    onClick = addDestination,
                    enabled = !busy,
                    modifier = Modifier.fillMaxWidth().testTag("folder-link-add-destination-${share.offerId}"),
                ) {
                    Text(stringResource(R.string.folder_link_add_destination))
                }
            }
            if (showLinkSettings && share.linkPolicy != null) {
                FolderLinkSettingsSummary(
                    state = share.linkSettings,
                    savedChange = savedSettingsChange,
                    busy = busy,
                    edit = editSettings,
                    retry = retrySettings,
                    review = reviewSettings,
                )
            }
            val summary = when (status.summaryFor(share)) {
                FolderShareSummary.REMOVAL_PENDING -> stringResource(R.string.folder_sync_removal_pending, peerName)
                FolderShareSummary.REMOVED -> stringResource(R.string.folder_sync_removed)
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
            if (share.phase != FolderSharePhase.REMOVED && share.expired) Text(stringResource(
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
                    OutlinedButton(onClick = { mutate("pause") }, enabled = !busy) {
                        Text(stringResource(if (share.linkPolicy == null) R.string.action_pause else R.string.folder_link_pause))
                    }
                }
                if (status.issue != FolderSyncIssue.FOLDER_ACCESS && share.phase == FolderSharePhase.PAUSED && !share.expired) {
                    OutlinedButton(onClick = { mutate("resume") }, enabled = !busy) {
                        Text(stringResource(if (share.linkPolicy == null) R.string.action_resume else R.string.folder_link_resume))
                    }
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

@Composable
private fun FolderLinkSettingsSummary(
    state: FolderLinkSettingsState?,
    savedChange: SavedFolderLinkSettingsChange?,
    busy: Boolean,
    edit: (FolderLinkSettingsState, FolderLinkPolicy) -> Unit,
    retry: (SavedFolderLinkSettingsChange) -> Unit,
    review: (FolderLinkSettingsState, FolderLinkPolicy) -> Unit,
) {
    if (state == null) {
        Text(stringResource(R.string.folder_link_settings_unavailable), color = MaterialTheme.colorScheme.error)
        return
    }
    Text(
        stringResource(R.string.folder_link_settings_revision, state.revision),
        fontWeight = FontWeight.SemiBold,
    )
    Text(
        stringResource(R.string.folder_link_settings_every_destination),
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
    if (!state.confirmed) {
        Text(stringResource(R.string.folder_link_settings_unconfirmed))
        return
    }
    state.conflictedChange?.let { conflict ->
        Text(stringResource(R.string.folder_link_settings_conflicted), color = MaterialTheme.colorScheme.error)
        LinkPolicySummary(conflict.settings.deletionPolicy)
        OutlinedButton(
            onClick = { review(state, conflict.settings.deletionPolicy) },
            enabled = !busy,
        ) { Text(stringResource(R.string.folder_link_settings_review)) }
        return
    }
    state.pendingChange?.let { pending ->
        Text(stringResource(R.string.folder_link_settings_waiting_for_source))
        LinkPolicySummary(pending.settings.deletionPolicy)
        return
    }
    savedChange?.let { saved ->
        Text(
            stringResource(
                if (saved.requiresReview) R.string.folder_link_settings_saved_review
                else R.string.folder_link_settings_status_unknown,
            ),
        )
        LinkPolicySummary(saved.settings.deletionPolicy)
        OutlinedButton(
            onClick = {
                if (saved.requiresReview) review(state, saved.settings.deletionPolicy)
                else retry(saved)
            },
            enabled = !busy,
        ) {
            Text(stringResource(
                if (saved.requiresReview) R.string.folder_link_settings_review
                else R.string.folder_link_settings_retry_exact,
            ))
        }
        return
    }
    if (state.settings.paused) Text(stringResource(R.string.folder_link_settings_paused))
    OutlinedButton(
        onClick = { edit(state, state.settings.deletionPolicy) },
        enabled = !busy,
        modifier = Modifier.fillMaxWidth(),
    ) { Text(stringResource(R.string.folder_link_settings_edit)) }
}

@Composable
private fun LinkPolicySummary(policy: FolderLinkPolicy) {
    Text(stringResource(
        if (policy.propagateSourceDeletions) R.string.folder_link_source_deletions_propagate
        else R.string.folder_link_source_deletions_keep,
    ))
    Text(stringResource(
        if (policy.restoreLocalDeletions) R.string.folder_link_destination_deletions_restore
        else R.string.folder_link_destination_deletions_keep,
    ))
}

@Composable
private fun LinkPolicyControl(
    checked: Boolean,
    onCheckedChange: (Boolean) -> Unit,
    title: String,
    detail: String,
    tag: String,
) {
    Card(Modifier.fillMaxWidth().testTag(tag)) {
        Row(
            Modifier.padding(16.dp),
            horizontalArrangement = Arrangement.spacedBy(12.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                Text(title, fontWeight = FontWeight.SemiBold)
                Text(detail, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
            Switch(checked = checked, onCheckedChange = onCheckedChange)
        }
    }
}

@Composable
private fun LinkSettingsEditorDialog(
    editor: LinkSettingsEditor,
    busy: Boolean,
    onChange: (LinkSettingsEditor) -> Unit,
    onSave: () -> Unit,
    onDismiss: () -> Unit,
) {
    AlertDialog(
        onDismissRequest = { if (!busy) onDismiss() },
        title = { Text(stringResource(R.string.folder_link_settings_edit_title, editor.label)) },
        text = {
            Column(
                Modifier.verticalScroll(rememberScrollState()),
                verticalArrangement = Arrangement.spacedBy(10.dp),
            ) {
                Text(stringResource(R.string.folder_link_settings_every_destination))
                LinkPolicyControl(
                    editor.propagateSourceDeletions,
                    { onChange(editor.copy(propagateSourceDeletions = it)) },
                    stringResource(R.string.folder_link_source_deletions),
                    stringResource(R.string.folder_link_source_deletions_detail),
                    "folder-link-settings-source-deletions",
                )
                LinkPolicyControl(
                    editor.restoreLocalDeletions,
                    { onChange(editor.copy(restoreLocalDeletions = it)) },
                    stringResource(R.string.folder_link_destination_deletions),
                    stringResource(R.string.folder_link_destination_deletions_detail),
                    "folder-link-settings-destination-deletions",
                )
            }
        },
        confirmButton = {
            Button(onClick = onSave, enabled = !busy) {
                Text(stringResource(R.string.folder_link_settings_save))
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss, enabled = !busy) {
                Text(stringResource(R.string.action_cancel))
            }
        },
    )
}

@Composable
private fun LinkSettingsConfirmationDialog(
    editor: LinkSettingsEditor,
    busy: Boolean,
    onConfirm: () -> Unit,
    onDismiss: () -> Unit,
) {
    val current = editor.current.settings.deletionPolicy
    AlertDialog(
        onDismissRequest = { if (!busy) onDismiss() },
        title = { Text(stringResource(R.string.folder_link_settings_confirm_title)) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
                if (!current.propagateSourceDeletions && editor.propagateSourceDeletions) {
                    Text(stringResource(R.string.folder_link_settings_confirm_source_deletions))
                }
                if (!current.restoreLocalDeletions && editor.restoreLocalDeletions) {
                    Text(stringResource(R.string.folder_link_settings_confirm_restore_deletions))
                }
                Text(stringResource(R.string.folder_link_settings_confirm_current_until_applied))
            }
        },
        confirmButton = {
            Button(onClick = onConfirm, enabled = !busy) {
                Text(stringResource(R.string.folder_link_settings_confirm))
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss, enabled = !busy) {
                Text(stringResource(R.string.action_cancel))
            }
        },
    )
}

private fun LinkSettingsEditor.proposedSettings() = FolderLinkSettings(
    FolderLinkPolicy(propagateSourceDeletions, restoreLocalDeletions),
    paused = current.settings.paused,
)

private fun folderSyncErrorText(error: Throwable): String = when (error) {
    is SecurityException -> "Android denied access to that folder."
    is IllegalArgumentException -> "That folder or request is not valid for sync."
    else -> "Folder sync needs attention. Refresh to check the latest status."
}
