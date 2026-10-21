package life.michaelwong.covalent.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.text.input.InputTransformation
import androidx.compose.foundation.text.input.TextFieldLineLimits
import androidx.compose.foundation.text.input.rememberTextFieldState
import androidx.compose.foundation.text.input.maxLength
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import life.michaelwong.covalent.R
import life.michaelwong.covalent.data.CovalentNodeClient
import life.michaelwong.covalent.model.DiscoveryCandidate
import life.michaelwong.covalent.model.FolderSyncAvailability
import life.michaelwong.covalent.model.FolderSyncIssue
import life.michaelwong.covalent.model.FolderSyncLifecycle
import life.michaelwong.covalent.model.FolderSyncStatus
import life.michaelwong.covalent.model.NetworkPairing
import life.michaelwong.covalent.model.NetworkPairingState
import life.michaelwong.covalent.model.NodeConnection
import life.michaelwong.covalent.node.EmbeddedNodeManager

internal enum class FolderSyncPairingBlocker { INSTALLATION, NEEDS_ATTENTION }

internal fun FolderSyncStatus.pairingBlocker(): FolderSyncPairingBlocker? = when {
    availability == FolderSyncAvailability.NOT_PACKAGED || issue == FolderSyncIssue.INSTALLATION ->
        FolderSyncPairingBlocker.INSTALLATION
    availability == FolderSyncAvailability.NEEDS_ATTENTION ||
        lifecycle == FolderSyncLifecycle.NEEDS_ATTENTION || issue != null ->
        FolderSyncPairingBlocker.NEEDS_ATTENTION
    else -> null
}

/** Pair only the on-phone sync identity; the separate backup connection is never selected or changed. */
@Composable
@OptIn(ExperimentalMaterial3Api::class)
internal fun FolderSyncPairing(manager: EmbeddedNodeManager, onPeersChanged: () -> Unit, photoAppliance: Boolean = false) {
    val client = remember { CovalentNodeClient() }
    val mutex = remember { Mutex() }
    val scope = rememberCoroutineScope()
    val peersChanged by rememberUpdatedState(onPeersChanged)
    val failureMessage = stringResource(R.string.folder_sync_pair_failure)
    val installationMessage = stringResource(R.string.folder_sync_pair_installation)
    val needsAttentionMessage = stringResource(R.string.folder_sync_pair_needs_attention)
    val address = rememberTextFieldState()
    var expanded by rememberSaveable { mutableStateOf(false) }
    var pending by remember { mutableStateOf<List<NetworkPairing>>(emptyList()) }
    var candidates by remember { mutableStateOf<List<DiscoveryCandidate>>(emptyList()) }
    var pairedIds by remember { mutableStateOf<Set<String>>(emptySet()) }
    var completedPairings by remember { mutableStateOf<Set<String>>(emptySet()) }
    var blocker by remember { mutableStateOf<FolderSyncPairingBlocker?>(null) }
    var discoveryFinished by remember { mutableStateOf(false) }
    var ready by remember { mutableStateOf(false) }
    var busy by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }

    fun localConnection(): NodeConnection = checkNotNull(manager.localConnectionForFolderSync())

    suspend fun refreshPairing() {
        val (requests, status) = withContext(Dispatchers.IO) {
            val connection = localConnection()
            val requests = client.pendingNetworkPairings(connection.baseUrl, connection.token)
            requests to client.folderSyncStatus(connection.baseUrl, connection.token)
        }
        val ids = status.peers.map { it.peerId }.toSet()
        pending = requests.sortedBy { it.expiresAtUnixMs }
        // A confirmation response can be lost after the peer commits it. Let
        // the authoritative completed state replace a stale network error.
        val completed = requests.filter { it.state == NetworkPairingState.COMPLETE }.map { it.pairingId }.toSet()
        if (completed.any { it !in completedPairings }) error = null
        completedPairings = completed
        blocker = status.pairingBlocker()
        ready = true
        if (pairedIds != ids) {
            if (ids.any { it !in pairedIds }) error = null
            pairedIds = ids
            peersChanged()
        }
    }

    fun perform(action: (NodeConnection) -> Unit) {
        if (busy) return
        busy = true
        error = null
        scope.launch {
            try {
                mutex.withLock {
                    withContext(Dispatchers.IO) { action(localConnection()) }
                    refreshPairing()
                }
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (_: Exception) {
                error = failureMessage
            } finally {
                busy = false
            }
        }
    }

    fun start(candidate: String) {
        val selectedAddress = candidate.trim()
        if (selectedAddress.isBlank()) return
        perform { connection ->
            client.startNetworkPairing(connection.baseUrl, connection.token, selectedAddress)
        }
    }

    LaunchedEffect(manager) {
        while (isActive) {
            try {
                mutex.withLock { refreshPairing() }
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (_: Exception) {
                ready = false
            }
            delay(2_000)
        }
    }

    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
            Text(if (photoAppliance) "Pair your Pi" else stringResource(R.string.folder_sync_pair_title), fontWeight = FontWeight.SemiBold)
            if (pairedIds.isNotEmpty() && pending.isEmpty()) {
                Text(pluralStringResource(R.plurals.folder_sync_devices_paired, pairedIds.size, pairedIds.size))
                TextButton(onClick = { expanded = !expanded }) {
                    Text(stringResource(if (expanded) R.string.action_cancel else R.string.folder_sync_connect_another))
                }
            }
            if (pairedIds.isEmpty() || pending.isNotEmpty() || expanded) {
            Text(if (photoAppliance) "Enter your Pi’s Tailscale address. Compare the code on both screens to pair." else stringResource(R.string.folder_sync_pair_detail))
            if (!ready) Text(stringResource(R.string.folder_sync_pair_waiting))
            OutlinedButton(
                enabled = ready && !busy,
                onClick = {
                    if (!busy) {
                        busy = true
                        error = null
                        scope.launch {
                            try {
                                mutex.withLock {
                                    candidates = withContext(Dispatchers.IO) {
                                        val connection = localConnection()
                                        client.discovery(connection.baseUrl, connection.token)
                                    }
                                    discoveryFinished = true
                                }
                            } catch (cancelled: CancellationException) {
                                throw cancelled
                            } catch (_: Exception) {
                                error = failureMessage
                            } finally {
                                busy = false
                            }
                        }
                    }
                },
            ) { Text(stringResource(R.string.action_find_devices)) }
            if (discoveryFinished && candidates.isEmpty()) {
                Text(stringResource(R.string.folder_sync_discovery_empty))
            }
            candidates.forEach { candidate ->
                OutlinedButton(enabled = ready && !busy, onClick = { start(candidate.endpoint) }) {
                    Text(candidate.endpoint)
                }
            }
            if (!photoAppliance) Text(stringResource(R.string.folder_sync_tailnet_hint))
            OutlinedTextField(
                state = address,
                inputTransformation = InputTransformation.maxLength(512),
                label = { Text(stringResource(R.string.folder_sync_pair_address)) },
                placeholder = { Text(stringResource(R.string.folder_sync_pair_address_example)) },
                keyboardOptions = KeyboardOptions(
                    capitalization = KeyboardCapitalization.None,
                    autoCorrectEnabled = false,
                    keyboardType = KeyboardType.Uri,
                ),
                lineLimits = TextFieldLineLimits.SingleLine,
                modifier = Modifier.fillMaxWidth().testTag("folder-pair.address"),
            )
            Button(
                enabled = ready && !busy && address.text.isNotBlank(),
                onClick = { start(address.text.toString()) },
                modifier = Modifier.testTag("folder-pair.start"),
            ) { Text(stringResource(R.string.folder_sync_pair_start)) }
            error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
            pending.forEach { pairing ->
                NetworkPairingCard(
                    pairing = pairing,
                    providerPersisted = pairing.state == NetworkPairingState.COMPLETE &&
                        pairing.peerTransport?.peerId?.let { it in pairedIds } == true,
                    busy = busy,
                    confirm = {
                        perform { connection ->
                            client.confirmNetworkPairing(
                                connection.baseUrl, connection.token,
                                pairing.pairingId, pairing.authenticationString,
                            )
                        }
                    },
                    dismiss = {
                        perform { connection ->
                            client.cancelNetworkPairing(connection.baseUrl, connection.token, pairing.pairingId)
                        }
                    },
                    forFolderSync = true,
                    completionFailure = when (blocker) {
                        FolderSyncPairingBlocker.INSTALLATION -> installationMessage
                        FolderSyncPairingBlocker.NEEDS_ATTENTION -> needsAttentionMessage
                        null -> null
                    },
                )
            }
            }
        }
    }
}
