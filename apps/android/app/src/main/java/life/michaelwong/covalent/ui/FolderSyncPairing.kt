package life.michaelwong.covalent.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
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
import life.michaelwong.covalent.model.NetworkPairing
import life.michaelwong.covalent.model.NetworkPairingState
import life.michaelwong.covalent.model.NodeConnection
import life.michaelwong.covalent.node.EmbeddedNodeManager

/** Pair only the on-phone sync identity; the separate backup connection is never selected or changed. */
@Composable
internal fun FolderSyncPairing(manager: EmbeddedNodeManager, onPeersChanged: () -> Unit) {
    val client = remember { CovalentNodeClient() }
    val mutex = remember { Mutex() }
    val scope = rememberCoroutineScope()
    val peersChanged by rememberUpdatedState(onPeersChanged)
    val failureMessage = stringResource(R.string.folder_sync_pair_failure)
    var address by remember { mutableStateOf("") }
    var pending by remember { mutableStateOf<List<NetworkPairing>>(emptyList()) }
    var candidates by remember { mutableStateOf<List<DiscoveryCandidate>>(emptyList()) }
    var pairedIds by remember { mutableStateOf<Set<String>>(emptySet()) }
    var discoveryFinished by remember { mutableStateOf(false) }
    var ready by remember { mutableStateOf(false) }
    var busy by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }

    fun localConnection(): NodeConnection = checkNotNull(manager.localConnectionForFolderSync())

    suspend fun refreshPairing() {
        val (requests, ids) = withContext(Dispatchers.IO) {
            val connection = localConnection()
            val requests = client.pendingNetworkPairings(connection.baseUrl, connection.token)
            requests to client.folderSyncStatus(connection.baseUrl, connection.token).peers.map { it.peerId }.toSet()
        }
        pending = requests.sortedBy { it.expiresAtUnixMs }
        ready = true
        if (pairedIds != ids) {
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
            Text(stringResource(R.string.folder_sync_pair_title), fontWeight = FontWeight.SemiBold)
            Text(stringResource(R.string.folder_sync_pair_detail))
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
            OutlinedTextField(
                value = address,
                onValueChange = { if (it.length <= 512) address = it },
                label = { Text(stringResource(R.string.folder_sync_pair_address)) },
                placeholder = { Text(stringResource(R.string.folder_sync_pair_address_example)) },
                keyboardOptions = KeyboardOptions(
                    capitalization = KeyboardCapitalization.None,
                    autoCorrectEnabled = false,
                    keyboardType = KeyboardType.Uri,
                ),
                singleLine = true,
                modifier = Modifier.fillMaxWidth().testTag("folder-pair.address"),
            )
            Button(
                enabled = ready && !busy && address.isNotBlank(),
                onClick = { start(address) },
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
                )
            }
        }
    }
}
