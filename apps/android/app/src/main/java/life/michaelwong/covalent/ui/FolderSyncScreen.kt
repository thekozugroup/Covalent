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
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import androidx.documentfile.provider.DocumentFile
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
import life.michaelwong.covalent.model.AndroidLinkConditions
import life.michaelwong.covalent.model.FolderLinkCadence
import life.michaelwong.covalent.model.FolderLinkRunPhase
import life.michaelwong.covalent.model.FolderLinkRunResult
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
import life.michaelwong.covalent.sync.SavedFolderLinkRunRequest
import life.michaelwong.covalent.sync.NodeFolderSyncApi
import life.michaelwong.covalent.sync.PeerAddressUpdateDraft
import life.michaelwong.covalent.sync.PeerAddressUpdateOutcome
import life.michaelwong.covalent.sync.SafFolderGrantStore
import life.michaelwong.covalent.sync.completePeerAddressUpdate

private data class LoadedFolderSyncStatus(
    val status: FolderSyncStatus,
    val pendingRepairOffers: Set<String>,
    val retiredFolderChoice: Boolean,
    val savedSettingsChanges: List<SavedFolderLinkSettingsChange>,
    val savedRunRequests: List<SavedFolderLinkRunRequest>,
)

private data class LinkSettingsEditor(
    val folderId: String,
    val label: String,
    val current: FolderLinkSettingsState,
    val propagateSourceDeletions: Boolean,
    val restoreLocalDeletions: Boolean,
    val cadence: FolderLinkCadence,
    val scheduledMinutes: String,
    val wifiOnly: Boolean,
    val chargingOnly: Boolean,
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
    val pairedDeviceName = stringResource(R.string.folder_sync_paired_device)
    val invalidPeerAddressMessage = stringResource(R.string.node_error_invalid_peer_address)
    val peerAddressChangedMessage = stringResource(R.string.node_error_peer_address_changed)
    val addressUpdatedMessage = stringResource(R.string.folder_sync_address_updated_checking)
    val addressSavedRefreshNeededMessage = stringResource(R.string.folder_sync_address_saved_refresh_needed)
    val settingsPendingMessage = stringResource(R.string.folder_link_settings_pending_error)
    val settingsConflictMessage = stringResource(R.string.folder_link_settings_conflict_error)
    val settingsUnknownMessage = stringResource(R.string.folder_link_settings_status_unknown)
    val runPendingMessage = stringResource(R.string.folder_link_run_pending_error)
    val runConflictMessage = stringResource(R.string.folder_link_run_conflict_error)
    val runUnknownMessage = stringResource(R.string.folder_link_run_status_unknown)
    val selectedFolderFallback = stringResource(R.string.folder_sync_selected_folder)
    val scope = androidx.compose.runtime.rememberCoroutineScope()
    val grants = remember(context) { FolderSyncGrantStore(context.applicationContext) }
    val safGrants = remember(context) { SafFolderGrantStore(context.applicationContext) }
    val settingsChanges = remember(context) { FolderLinkSettingsChangeStore(context.applicationContext) }
    val api = remember { NodeFolderSyncApi(CovalentNodeClient()) }
    var hostRequested by remember { mutableStateOf(manager.folderSyncRequested()) }
    var status by remember { mutableStateOf<FolderSyncStatus?>(null) }
    var error by remember { mutableStateOf<String?>(null) }
    var selectedFolder by remember { mutableStateOf<String?>(null) }
    var selectedFolderLabel by remember { mutableStateOf<String?>(null) }
    var label by remember { mutableStateOf("") }
    var selectedPeer by remember { mutableStateOf<String?>(null) }
    var propagateSourceDeletions by remember { mutableStateOf(false) }
    var restoreLocalDeletions by remember { mutableStateOf(false) }
    var cadence by remember { mutableStateOf<FolderLinkCadence>(FolderLinkCadence.Continuous) }
    var scheduledMinutes by remember { mutableStateOf("60") }
    var wifiOnly by remember { mutableStateOf(false) }
    var chargingOnly by remember { mutableStateOf(false) }
    var destinationSource by remember { mutableStateOf<FolderShare?>(null) }
    var busy by remember { mutableStateOf(false) }
    var pendingRepairOffers by remember { mutableStateOf<Set<String>>(emptySet()) }
    var savedSettingsChanges by remember { mutableStateOf<List<SavedFolderLinkSettingsChange>>(emptyList()) }
    var savedRunRequests by remember { mutableStateOf<List<SavedFolderLinkRunRequest>>(emptyList()) }
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
                settingsChanges.runRecords(),
            )
        }
    }

    fun applyStatus(loaded: LoadedFolderSyncStatus) {
        status = loaded.status
        pendingRepairOffers = loaded.pendingRepairOffers
        savedSettingsChanges = loaded.savedSettingsChanges
        savedRunRequests = loaded.savedRunRequests
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
        safGrants::requireSelectedRoot,
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

    fun submitRun(
        folderId: String,
        expectedGeneration: Long,
        settingsRevision: Long,
        retry: SavedFolderLinkRunRequest? = null,
    ) {
        if (busy) return
        busy = true
        error = null
        scope.launch {
            val prepared = runCatching { withContext(Dispatchers.IO) {
                retry ?: settingsChanges.prepareRun(folderId, expectedGeneration, settingsRevision)
            } }
            if (prepared.isFailure) {
                error = folderSyncErrorText(checkNotNull(prepared.exceptionOrNull()))
                busy = false
                return@launch
            }
            val request = checkNotNull(prepared.getOrNull())
            val persisted = runCatching { withContext(Dispatchers.IO) { settingsChanges.runRecords() } }
            if (persisted.isFailure) {
                error = folderSyncErrorText(checkNotNull(persisted.exceptionOrNull()))
                busy = false
                return@launch
            }
            savedRunRequests = checkNotNull(persisted.getOrNull())
            val submission = runCatching { withContext(Dispatchers.IO) {
                api.run(connection(), request.folderId, request.requestId,
                    request.expectedGeneration, request.settingsRevision)
            } }
            val failure = submission.exceptionOrNull()
            val code = (failure as? NodeApiException)?.code
            val storage = runCatching { withContext(Dispatchers.IO) {
                if (submission.isSuccess) settingsChanges.finishRun(request.folderId, request.requestId)
                else if (code == "link_run_pending" || code == "link_run_conflict") {
                    settingsChanges.markRunForReview(request.folderId, request.requestId)
                }
            } }
            val loaded = runCatching { loadStatus() }
            loaded.onSuccess(::applyStatus)
            val saved = runCatching { withContext(Dispatchers.IO) { settingsChanges.runRecords() } }
            saved.onSuccess { savedRunRequests = it }
            error = when {
                storage.isFailure -> folderSyncErrorText(checkNotNull(storage.exceptionOrNull()))
                code == "link_run_pending" -> runPendingMessage
                code == "link_run_conflict" -> runConflictMessage
                failure != null || loaded.isFailure || saved.isFailure -> runUnknownMessage
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
    val folderPicker = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocumentTree()) { uri ->
        if (uri == null) return@rememberLauncherForActivityResult
        busy = true
        scope.launch {
            runCatching { withContext(Dispatchers.IO) { safGrants.persist(uri) } }
                .onSuccess { grant ->
                    selectedFolder = grant.selectedRoot
                    selectedFolderLabel = DocumentFile.fromTreeUri(context, uri)?.name
                        ?: selectedFolderFallback
                    manager.refreshFolderSyncAccess()
                }
                .onFailure { error = folderSyncErrorText(it) }
            busy = false
            refresh()
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
        if (!hostRequested) {
            item {
                Card(Modifier.fillMaxWidth()) {
                    Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
                        Text(stringResource(R.string.folder_sync_access_title), fontWeight = FontWeight.SemiBold)
                        Text(stringResource(R.string.folder_sync_saf_access_explanation))
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
        item(key = "phone-pairing") {
            FolderSyncPairing(manager = manager, onPeersChanged = ::refresh)
        }
        item {
            FolderChooser(
                selected = selectedFolderLabel,
                busy = busy,
                onChoose = { folderPicker.launch(null) },
            )
        }
        val snapshot = status
        if (snapshot != null) {
            if (snapshot.issue != FolderSyncIssue.FOLDER_ACCESS) {
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
            if (snapshot.issue != FolderSyncIssue.FOLDER_ACCESS) {
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
                        LinkCadenceControl(
                            cadence,
                            scheduledMinutes,
                            { cadence = it },
                            { scheduledMinutes = it.filter(Char::isDigit).take(6) },
                        )
                    }
                    item {
                        AndroidConditionsControl(
                            wifiOnly,
                            chargingOnly,
                            { wifiOnly = it },
                            { chargingOnly = it },
                        )
                    }
                    item {
                        Button(
                            enabled = !busy && selectedFolder != null && selectedPeer != null && label.isNotBlank() &&
                                (cadence !is FolderLinkCadence.Scheduled || scheduledMinutes.toIntOrNull() in 15..525_600),
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
                                                FolderLinkSettings(
                                                    deletionPolicy = FolderLinkPolicy(
                                                        propagateSourceDeletions = propagateSourceDeletions,
                                                        restoreLocalDeletions = restoreLocalDeletions,
                                                    ),
                                                    paused = false,
                                                    cadence = if (cadence is FolderLinkCadence.Scheduled) {
                                                        FolderLinkCadence.Scheduled(checkNotNull(scheduledMinutes.toIntOrNull()))
                                                    } else cadence,
                                                    androidConditions = AndroidLinkConditions(wifiOnly, chargingOnly),
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
                    hasPendingRepair = share.offerId in pendingRepairOffers,
                    busy = busy,
                    addDestination = { destinationSource = share },
                    showLinkSettings = share.offerId == settingsCardOfferId,
                    savedSettingsChange = savedSettingsChanges.singleOrNull { it.folderId == share.folderId },
                    savedRunRequest = savedRunRequests.singleOrNull { it.folderId == share.folderId },
                    editSettings = { state, policy ->
                        settingsEditor = LinkSettingsEditor(
                            share.folderId,
                            share.label,
                            state,
                            policy.propagateSourceDeletions,
                            policy.restoreLocalDeletions,
                            state.settings.cadence,
                            (state.settings.cadence as? FolderLinkCadence.Scheduled)?.intervalMinutes?.toString() ?: "60",
                            state.settings.androidConditions.wifiOnly,
                            state.settings.androidConditions.chargingOnly,
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
                                    state.settings.cadence,
                                    (state.settings.cadence as? FolderLinkCadence.Scheduled)?.intervalMinutes?.toString() ?: "60",
                                    state.settings.androidConditions.wifiOnly,
                                    state.settings.androidConditions.chargingOnly,
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
                    runNow = { saved ->
                        val settings = checkNotNull(share.linkSettings)
                        submitRun(
                            share.folderId,
                            saved?.expectedGeneration ?: (share.linkRun?.generation ?: 0),
                            saved?.settingsRevision ?: settings.revision,
                            saved,
                        )
                    },
                    reviewRun = {
                        scope.launch {
                            runCatching {
                                withContext(Dispatchers.IO) { settingsChanges.takeRunForReview(share.folderId) }
                                withContext(Dispatchers.IO) { settingsChanges.runRecords() }
                            }.onSuccess {
                                savedRunRequests = it
                                submitRun(
                                    share.folderId,
                                    share.linkRun?.generation ?: 0,
                                    checkNotNull(share.linkSettings).revision,
                                )
                            }
                                .onFailure { error = folderSyncErrorText(it) }
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
    selected: String?,
    busy: Boolean,
    onChoose: () -> Unit,
) {
    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(stringResource(R.string.folder_sync_choose_folder), fontWeight = FontWeight.SemiBold)
            Text(selected ?: stringResource(R.string.folder_sync_no_folder))
            Button(onClick = onChoose, enabled = !busy) {
                Text(stringResource(R.string.folder_sync_use_folder))
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
    hasPendingRepair: Boolean,
    busy: Boolean,
    addDestination: () -> Unit,
    showLinkSettings: Boolean,
    savedSettingsChange: SavedFolderLinkSettingsChange?,
    savedRunRequest: SavedFolderLinkRunRequest?,
    editSettings: (FolderLinkSettingsState, FolderLinkPolicy) -> Unit,
    retrySettings: (SavedFolderLinkSettingsChange) -> Unit,
    reviewSettings: (FolderLinkSettingsState, FolderLinkPolicy) -> Unit,
    mutate: (String) -> Unit,
    runNow: (SavedFolderLinkRunRequest?) -> Unit,
    reviewRun: () -> Unit,
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
                FolderLinkRunSummary(
                    status,
                    share,
                    savedRunRequest,
                    busy,
                    runNow,
                    reviewRun,
                )
            }
            val shareSummary = status.summaryFor(share)
            val summary = when (shareSummary) {
                FolderShareSummary.REMOVAL_PENDING -> stringResource(R.string.folder_sync_removal_pending, peerName)
                FolderShareSummary.REMOVED -> stringResource(R.string.folder_sync_removed)
                FolderShareSummary.INVITATION_EXPIRED -> stringResource(R.string.folder_sync_invitation_expired)
                FolderShareSummary.PAUSED -> stringResource(R.string.folder_sync_connection_paused)
                FolderShareSummary.CHECKING -> stringResource(R.string.folder_sync_connection_checking)
                FolderShareSummary.NEEDS_ATTENTION -> stringResource(R.string.folder_sync_connection_attention)
                FolderShareSummary.WAITING_FOR_OTHER_DEVICE -> stringResource(R.string.folder_sync_waiting_other_device)
                FolderShareSummary.WAITING_FOR_CONDITIONS -> stringResource(R.string.folder_link_waiting_conditions)
                FolderShareSummary.OFFLINE -> stringResource(R.string.folder_sync_connection_offline)
                FolderShareSummary.SYNCING -> stringResource(R.string.folder_sync_connection_syncing)
                FolderShareSummary.WAITING_FOR_PEER -> stringResource(R.string.folder_sync_waiting_for_device, peerName)
                FolderShareSummary.CONNECTED -> stringResource(R.string.folder_sync_connected_to_device, peerName)
                FolderShareSummary.CONNECTION_UNKNOWN -> stringResource(R.string.folder_sync_connection_unknown)
                FolderShareSummary.READY -> stringResource(R.string.folder_link_run_ready)
            }
            Text(summary)
            if (shareSummary == FolderShareSummary.WAITING_FOR_CONDITIONS) {
                Text(
                    stringResource(R.string.folder_link_waiting_conditions_detail),
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            if (share.pairingUpgradeRequired) {
                Text(
                    stringResource(R.string.folder_sync_pairing_upgrade),
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            if (share.phase != FolderSharePhase.REMOVED && share.expired) Text(stringResource(
                if (share.incoming) R.string.folder_sync_renew_incoming_detail else R.string.folder_sync_renew_detail,
            ), color = MaterialTheme.colorScheme.onSurfaceVariant)
            val folderAccessUnavailable = status.issue == FolderSyncIssue.FOLDER_ACCESS ||
                status.folders.singleOrNull { it.folderId == share.folderId }?.accessUnavailable == true
            if (folderAccessUnavailable && share.phase != FolderSharePhase.REMOVED) {
                Text(
                    stringResource(R.string.folder_sync_choose_again_detail),
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
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
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                if (!folderAccessUnavailable && !share.incoming && share.expired &&
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
private fun FolderLinkRunSummary(
    status: FolderSyncStatus,
    share: FolderShare,
    saved: SavedFolderLinkRunRequest?,
    busy: Boolean,
    runNow: (SavedFolderLinkRunRequest?) -> Unit,
    review: () -> Unit,
) {
    val settings = share.linkSettings ?: return
    if (!settings.confirmed) return
    val run = share.linkRun
    Text(
        when (run?.phase) {
            FolderLinkRunPhase.PREPARING -> stringResource(R.string.folder_link_run_preparing)
            FolderLinkRunPhase.RUNNING -> stringResource(R.string.folder_link_run_running)
            FolderLinkRunPhase.SUCCEEDED -> stringResource(R.string.folder_link_run_succeeded)
            FolderLinkRunPhase.INCOMPLETE -> stringResource(R.string.folder_link_run_incomplete)
            FolderLinkRunPhase.INTERRUPTED -> stringResource(R.string.folder_link_run_interrupted)
            FolderLinkRunPhase.CANCELLED -> stringResource(R.string.folder_link_run_cancelled)
            null -> when (settings.settings.cadence) {
                FolderLinkCadence.Manual -> stringResource(R.string.folder_link_run_manual_idle)
                FolderLinkCadence.Continuous -> stringResource(R.string.folder_link_run_continuous)
                is FolderLinkCadence.Scheduled -> stringResource(R.string.folder_link_run_waiting_schedule)
            }
        },
        fontWeight = FontWeight.SemiBold,
    )
    if (share.incoming && settings.settings.cadence is FolderLinkCadence.Scheduled) {
        Text(stringResource(R.string.folder_link_run_source_owns_schedule))
    } else if (!share.incoming && run?.nextDueAtUnixMs != null) {
        Text(stringResource(
            R.string.folder_link_run_next_due,
            java.text.DateFormat.getDateTimeInstance().format(java.util.Date(run.nextDueAtUnixMs)),
        ))
    }
    run?.pendingRequest?.let { Text(stringResource(R.string.folder_link_run_pending)) }
    run?.rejectedRequest?.let { Text(
        stringResource(R.string.folder_link_run_stale),
        color = MaterialTheme.colorScheme.error,
    ) }
    run?.destinations?.forEach { destination ->
        Text(stringResource(
            R.string.folder_link_run_destination,
            status.peers.firstOrNull { it.peerId == destination.peerId }?.displayName
                ?: stringResource(if (share.incoming) R.string.folder_link_this_phone else R.string.folder_sync_paired_device),
            when (destination.result) {
                FolderLinkRunResult.PENDING -> stringResource(R.string.folder_link_run_result_pending)
                FolderLinkRunResult.SUCCEEDED -> stringResource(R.string.folder_link_run_result_succeeded)
                FolderLinkRunResult.FAILED -> stringResource(R.string.folder_link_run_result_failed)
                FolderLinkRunResult.TIMED_OUT -> stringResource(R.string.folder_link_run_result_timed_out)
                FolderLinkRunResult.INTERRUPTED -> stringResource(R.string.folder_link_run_result_interrupted)
                FolderLinkRunResult.CANCELLED -> stringResource(R.string.folder_link_run_result_cancelled)
            },
        ))
    }
    saved?.let {
        Text(stringResource(
            if (it.requiresReview) R.string.folder_link_run_saved_review
            else R.string.folder_link_run_status_unknown,
        ))
    }
    val active = run?.phase in setOf(FolderLinkRunPhase.PREPARING, FolderLinkRunPhase.RUNNING)
    if (!settings.settings.paused && share.phase == FolderSharePhase.READY && !active) {
        OutlinedButton(
            onClick = { if (saved?.requiresReview == true) review() else runNow(saved) },
            enabled = !busy && run?.pendingRequest == null,
            modifier = Modifier.fillMaxWidth().testTag("folder-link-run-${share.folderId}"),
        ) { Text(stringResource(
            if (saved?.requiresReview == true) R.string.folder_link_run_new_request
            else if (saved != null) R.string.folder_link_run_retry_exact
            else R.string.folder_link_run_now,
        )) }
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
        stringResource(R.string.folder_link_settings_every_destination),
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
    if (!state.confirmed) {
        Text(stringResource(R.string.folder_link_settings_unconfirmed))
        return
    }
    state.conflictedChange?.let { conflict ->
        Text(stringResource(R.string.folder_link_settings_conflicted), color = MaterialTheme.colorScheme.error)
        LinkSettingsSummary(conflict.settings)
        OutlinedButton(
            onClick = { review(state, conflict.settings.deletionPolicy) },
            enabled = !busy,
        ) { Text(stringResource(R.string.folder_link_settings_review)) }
        return
    }
    state.pendingChange?.let { pending ->
        Text(stringResource(R.string.folder_link_settings_waiting_for_source))
        LinkSettingsSummary(pending.settings)
        return
    }
    savedChange?.let { saved ->
        Text(
            stringResource(
                if (saved.requiresReview) R.string.folder_link_settings_saved_review
                else R.string.folder_link_settings_status_unknown,
            ),
        )
        LinkSettingsSummary(saved.settings)
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
    LinkSettingsSummary(state.settings, includePolicy = false)
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
private fun LinkSettingsSummary(settings: FolderLinkSettings, includePolicy: Boolean = true) {
    if (includePolicy) LinkPolicySummary(settings.deletionPolicy)
    Text(when (val cadence = settings.cadence) {
        FolderLinkCadence.Manual -> stringResource(R.string.folder_link_cadence_manual)
        FolderLinkCadence.Continuous -> stringResource(R.string.folder_link_cadence_continuous)
        is FolderLinkCadence.Scheduled -> pluralStringResource(
            R.plurals.folder_link_cadence_scheduled_every,
            cadence.intervalMinutes,
            cadence.intervalMinutes,
        )
    })
    if (settings.androidConditions.wifiOnly || settings.androidConditions.chargingOnly) {
        Text(stringResource(R.string.folder_link_android_conditions_summary,
            if (settings.androidConditions.wifiOnly) stringResource(R.string.folder_link_wifi_only_short) else "",
            if (settings.androidConditions.chargingOnly) stringResource(R.string.folder_link_charging_only_short) else "",
        ))
    }
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
private fun LinkCadenceControl(
    cadence: FolderLinkCadence,
    scheduledMinutes: String,
    onCadenceChange: (FolderLinkCadence) -> Unit,
    onScheduledMinutesChange: (String) -> Unit,
) {
    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(stringResource(R.string.folder_link_cadence_title), fontWeight = FontWeight.SemiBold)
            Text(stringResource(R.string.folder_link_cadence_detail), color = MaterialTheme.colorScheme.onSurfaceVariant)
            listOf(
                FolderLinkCadence.Manual to R.string.folder_link_cadence_manual,
                FolderLinkCadence.Continuous to R.string.folder_link_cadence_continuous,
            ).forEach { (value, text) ->
                OutlinedButton(
                    onClick = { onCadenceChange(value) },
                    enabled = cadence != value,
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Text(stringResource(text))
                }
            }
            OutlinedButton(
                onClick = { onCadenceChange(FolderLinkCadence.Scheduled(scheduledMinutes.toIntOrNull()?.coerceIn(15, 525_600) ?: 60)) },
                enabled = cadence !is FolderLinkCadence.Scheduled,
                modifier = Modifier.fillMaxWidth(),
            ) { Text(stringResource(R.string.folder_link_cadence_scheduled)) }
            if (cadence is FolderLinkCadence.Scheduled) {
                Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    listOf(15 to R.string.folder_link_cadence_15_minutes,
                        60 to R.string.folder_link_cadence_hourly,
                        1440 to R.string.folder_link_cadence_daily).forEach { (minutes, text) ->
                        TextButton(onClick = {
                            onCadenceChange(FolderLinkCadence.Scheduled(minutes))
                            onScheduledMinutesChange(minutes.toString())
                        }) { Text(stringResource(text)) }
                    }
                }
                OutlinedTextField(
                    value = scheduledMinutes,
                    onValueChange = {
                        onScheduledMinutesChange(it)
                    },
                    label = { Text(stringResource(R.string.folder_link_cadence_interval_minutes)) },
                    isError = scheduledMinutes.toIntOrNull() !in 15..525_600,
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
                    modifier = Modifier.fillMaxWidth().testTag("folder-link-scheduled-minutes"),
                )
            }
        }
    }
}

@Composable
private fun AndroidConditionsControl(
    wifiOnly: Boolean,
    chargingOnly: Boolean,
    onWifiOnlyChange: (Boolean) -> Unit,
    onChargingOnlyChange: (Boolean) -> Unit,
) {
    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(stringResource(R.string.folder_link_android_conditions_title), fontWeight = FontWeight.SemiBold)
            Text(stringResource(R.string.folder_link_android_conditions_detail), color = MaterialTheme.colorScheme.onSurfaceVariant)
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(stringResource(R.string.folder_link_wifi_only), Modifier.weight(1f))
                Switch(wifiOnly, onWifiOnlyChange)
            }
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(stringResource(R.string.folder_link_charging_only), Modifier.weight(1f))
                Switch(chargingOnly, onChargingOnlyChange)
            }
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
                LinkCadenceControl(
                    editor.cadence,
                    editor.scheduledMinutes,
                    { onChange(editor.copy(cadence = it)) },
                    { onChange(editor.copy(scheduledMinutes = it)) },
                )
                AndroidConditionsControl(
                    editor.wifiOnly,
                    editor.chargingOnly,
                    { onChange(editor.copy(wifiOnly = it)) },
                    { onChange(editor.copy(chargingOnly = it)) },
                )
            }
        },
        confirmButton = {
            Button(
                onClick = onSave,
                enabled = !busy && (editor.cadence !is FolderLinkCadence.Scheduled ||
                    editor.scheduledMinutes.toIntOrNull() in 15..525_600),
            ) {
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
    cadence = if (cadence is FolderLinkCadence.Scheduled) {
        FolderLinkCadence.Scheduled(checkNotNull(scheduledMinutes.toIntOrNull()))
    } else cadence,
    androidConditions = AndroidLinkConditions(wifiOnly, chargingOnly),
)

private fun folderSyncErrorText(error: Throwable): String = when (error) {
    is SecurityException -> "Android denied access to that folder."
    is IllegalArgumentException -> "That folder or request is not valid for sync."
    else -> "Folder sync needs attention. Refresh to check the latest status."
}
