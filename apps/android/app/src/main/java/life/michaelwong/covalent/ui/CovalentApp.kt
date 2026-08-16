package life.michaelwong.covalent.ui

import android.content.Intent
import android.net.Uri
import android.os.Build
import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.rounded.ArrowBack
import androidx.compose.material.icons.rounded.AddLink
import androidx.compose.material.icons.rounded.Backup
import androidx.compose.material.icons.rounded.CheckCircle
import androidx.compose.material.icons.rounded.CloudOff
import androidx.compose.material.icons.rounded.FolderOpen
import androidx.compose.material.icons.rounded.Refresh
import androidx.compose.material.icons.rounded.Security
import androidx.compose.material.icons.rounded.Settings
import androidx.compose.material.icons.rounded.Storage
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.AssistChip
import androidx.compose.material3.Card
import androidx.compose.material3.Checkbox
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalFocusManager
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import androidx.documentfile.provider.DocumentFile
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import life.michaelwong.covalent.data.CovalentNodeClient
import life.michaelwong.covalent.data.NodeApiException
import life.michaelwong.covalent.data.SecureNodeStore
import life.michaelwong.covalent.data.newId
import life.michaelwong.covalent.model.DiscoveryCandidate
import life.michaelwong.covalent.model.NodeStatus
import life.michaelwong.covalent.model.PrimaryAction
import life.michaelwong.covalent.model.Provider
import life.michaelwong.covalent.model.RememberedBackup
import life.michaelwong.covalent.ui.theme.CovalentTheme
import life.michaelwong.covalent.work.TransferScheduler
import life.michaelwong.covalent.work.TransferWorker
import org.json.JSONArray
import org.json.JSONObject

internal enum class Screen { HOME, SETUP, PAIR, BACKUP, RESTORE, SETTINGS }

internal fun Screen.systemBackTarget(): Screen? = when (this) {
    Screen.PAIR, Screen.BACKUP, Screen.RESTORE, Screen.SETTINGS -> Screen.HOME
    Screen.HOME, Screen.SETUP -> null
}

internal val pairingInvitationKeyboardOptions = KeyboardOptions(
    capitalization = KeyboardCapitalization.None,
    autoCorrectEnabled = false,
    // Password keyboards disable IME suggestions and composition entirely. The field remains
    // visibly plain text because keyboard type and visual transformation are independent.
    keyboardType = KeyboardType.Password,
    imeAction = ImeAction.Done,
)

internal data class ValidatedSetup(
    val status: NodeStatus,
    val backups: List<RememberedBackup>,
)

internal fun validateAndPersistSetup(
    node: CovalentNodeClient,
    store: SecureNodeStore,
    displayName: String,
    address: String,
    token: String,
): ValidatedSetup {
    val status = node.status(address)
    val backups = node.backups(address, token)
    store.displayName = displayName
    store.baseUrl = address
    store.token = token
    store.replaceBackups(backups)
    return ValidatedSetup(status, backups)
}

internal fun shouldReturnToSetupAfterRefreshFailure(error: Throwable): Boolean =
    error is NodeApiException && error.statusCode == 401

internal fun startupRefreshDispatcher(): CoroutineDispatcher = Dispatchers.IO

private const val LOCAL_NETWORK_PERMISSION = "android.permission.ACCESS_LOCAL_NETWORK"

private class AppState(private val store: SecureNodeStore) {
    var screen by mutableStateOf(if (store.baseUrl.isBlank()) Screen.SETUP else Screen.HOME)
    var status by mutableStateOf<NodeStatus?>(null)
    var message by mutableStateOf<String?>(null)
    var busy by mutableStateOf(false)
    var providers by mutableStateOf(emptyList<Provider>())
    var discovered by mutableStateOf(emptyList<DiscoveryCandidate>())
    var backups by mutableStateOf(store.rememberedBackups())
    var selectedSource by mutableStateOf<Uri?>(null)
    var selectedTarget by mutableStateOf<Uri?>(null)
    var restorePlan by mutableStateOf<JSONObject?>(null)
    var pairingSession by mutableStateOf<JSONObject?>(null)
    fun refreshBackups() { backups = store.rememberedBackups() }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun CovalentApp(storeOverride: SecureNodeStore? = null) {
    val context = LocalContext.current
    val store = remember(storeOverride) { storeOverride ?: SecureNodeStore(context.applicationContext) }
    val node = remember { CovalentNodeClient() }
    val state = remember { AppState(store) }
    val scope = rememberCoroutineScope()
    val sourcePicker = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocumentTree()) { uri ->
        uri?.let {
            context.contentResolver.takePersistableUriPermission(it, Intent.FLAG_GRANT_READ_URI_PERMISSION)
            state.selectedSource = it
            state.message = "Folder access saved. Covalent will report if this access is later revoked."
        }
    }
    val targetPicker = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocumentTree()) { uri ->
        uri?.let {
            context.contentResolver.takePersistableUriPermission(it, Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION)
            state.selectedTarget = it
        }
    }
    val createSettings = rememberLauncherForActivityResult(ActivityResultContracts.CreateDocument("application/json")) { uri ->
        if (uri == null) return@rememberLauncherForActivityResult
        api(state, scope) {
            val exported = node.post(store.baseUrl, store.token, "/api/v1/config/export", JSONObject())
            context.contentResolver.openOutputStream(uri)?.bufferedWriter()?.use { it.write(exported.toString(2)) }
                ?: error("The selected file is not writable.")
            state.message = "Safe settings exported. Private identity keys were not included."
        }
    }
    val importSettings = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
        if (uri == null) return@rememberLauncherForActivityResult
        api(state, scope) {
            val text = context.contentResolver.openInputStream(uri)?.bufferedReader()?.use { it.readText() }
                ?: error("The selected file cannot be read.")
            node.postNoContent(store.baseUrl, store.token, "/api/v1/config/import", JSONObject()
                .put("confirmed", true).put("settings", JSONObject(text)))
            state.message = "Settings imported after explicit confirmation. Keys and credentials remain local."
            refreshStatus(state, node, store)
        }
    }
    val localNetworkPermission = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        if (granted) {
            api(state, scope) { setLanDiscovery(state, node, store, true) }
        } else {
            state.message = "Local network access was not granted. Discovery remains off; manual pairing still works."
        }
    }
    LaunchedEffect(store.baseUrl) {
        if (store.baseUrl.isNotBlank()) {
            runCatching {
                withContext(startupRefreshDispatcher()) { refreshStatus(state, node, store) }
            }.onFailure { error ->
                if (shouldReturnToSetupAfterRefreshFailure(error)) state.screen = Screen.SETUP
            }
        }
    }
    LaunchedEffect(Unit) {
        runCatching {
            withContext(Dispatchers.IO) { TransferScheduler.requeuePending(context, store) }
        }.onFailure {
            state.message = it.message ?: "Android could not resume a pending transfer."
        }
    }
    BackHandler(enabled = state.screen.systemBackTarget() != null) {
        state.screen.systemBackTarget()?.let { state.screen = it }
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(if (state.screen == Screen.HOME) "Covalent" else state.screen.name.lowercase().replaceFirstChar(Char::titlecase)) },
                navigationIcon = if (state.screen != Screen.HOME && state.screen != Screen.SETUP) {
                    { IconButton(onClick = { state.screen = Screen.HOME }) { Icon(Icons.AutoMirrored.Rounded.ArrowBack, "Back") } }
                } else ({}) ,
                actions = {
                    if (state.screen == Screen.HOME) {
                        IconButton(onClick = { state.screen = Screen.SETTINGS }) {
                            Icon(Icons.Rounded.Settings, "Settings")
                        }
                    }
                },
                colors = TopAppBarDefaults.topAppBarColors(containerColor = MaterialTheme.colorScheme.surface),
            )
        },
        floatingActionButton = {
            if (state.screen == Screen.HOME) {
                PrimaryActionToolbar(enabled = state.status?.state == "ready") { action ->
                    state.screen = when (action) {
                        PrimaryAction.PAIR -> Screen.PAIR
                        PrimaryAction.BACKUP -> Screen.BACKUP
                        PrimaryAction.RESTORE -> Screen.RESTORE
                    }
                }
            }
        },
        floatingActionButtonPosition = androidx.compose.material3.FabPosition.Center,
    ) { innerPadding ->
        Box(Modifier.fillMaxSize().padding(innerPadding), contentAlignment = Alignment.TopCenter) {
            val page = Modifier.widthIn(max = 860.dp).fillMaxSize()
            when (state.screen) {
                Screen.HOME -> Home(state, node, store, scope, page)
                Screen.SETUP -> Setup(state, node, store, scope, page)
                Screen.PAIR -> Pair(state, node, store, scope, page)
                Screen.BACKUP -> Backup(state, node, store, scope, page) { sourcePicker.launch(null) }
                Screen.RESTORE -> Restore(state, node, store, scope, page) { targetPicker.launch(null) }
                Screen.SETTINGS -> Settings(state, node, store, scope, page, createSettings::launch, importSettings::launch) { enabled ->
                    if (enabled && Build.VERSION.SDK_INT >= 37) localNetworkPermission.launch(LOCAL_NETWORK_PERMISSION)
                    else api(state, scope) { setLanDiscovery(state, node, store, enabled) }
                }
            }
        }
    }
    state.message?.let { message ->
        AlertDialog(onDismissRequest = { state.message = null }, confirmButton = {
            FilledTonalButton(onClick = { state.message = null }) { Text("OK") }
        }, title = { Text("Covalent") }, text = { Text(message) })
    }
}

@Composable
private fun Home(state: AppState, node: CovalentNodeClient, store: SecureNodeStore, scope: kotlinx.coroutines.CoroutineScope, modifier: Modifier) {
    LazyColumn(modifier, contentPadding = PaddingValues(20.dp, 14.dp, 20.dp, 112.dp), verticalArrangement = Arrangement.spacedBy(16.dp)) {
        item { Column {
            Text("Your copies. Your devices.", style = MaterialTheme.typography.headlineMedium, fontWeight = FontWeight.SemiBold)
            Spacer(Modifier.height(8.dp))
            Text("Pair directly, choose every replica, and restore only beneath a folder you authorize.", color = MaterialTheme.colorScheme.onSurfaceVariant)
        } }
        item { StatusCard("Local node", state.status?.deviceName ?: "Not connected", state.status?.let { "Ready on protocol ${it.protocolVersion}." } ?: "Connect to a running local Covalent node to enable actions.", Icons.Rounded.Storage) }
        item { StatusCard("Replica policy", "Explicit selection", "Covalent never selects a storage device for you.", Icons.Rounded.Security) }
        item { Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.fillMaxWidth()) {
            Text("Remembered backups", style = MaterialTheme.typography.titleLarge, modifier = Modifier.weight(1f))
            IconButton(onClick = { api(state, scope) { refreshStatus(state, node, store); state.refreshBackups() } }) { Icon(Icons.Rounded.Refresh, "Refresh") }
        } }
        if (state.backups.isEmpty()) item { EmptyState("No completed backups remembered on this device.", "Create a backup after choosing a folder and replicas.") }
        items(state.backups.size) { index ->
            val backup = state.backups[index]
            Card(Modifier.fillMaxWidth()) { Column(Modifier.padding(18.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                Text(backup.name, style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold)
                Text(
                    backup.latestSnapshotId?.let { "Latest snapshot $it · ${backup.snapshotCount} retained" }
                        ?: "Remembered definition · no local snapshot",
                    style = MaterialTheme.typography.bodySmall,
                )
                backup.latestSnapshotId?.let { snapshotId ->
                    OutlinedButton(onClick = {
                        api(state, scope) {
                            val response = node.post(store.baseUrl, store.token, "/api/v1/backups/verify", JSONObject()
                                .put("backupId", backup.backupId).put("snapshotId", snapshotId).put("verifyProviders", true))
                            state.message = if (response.getBoolean("intact")) "Verified: all checked chunks are intact." else "Verification found unavailable or damaged chunks."
                        }
                    }) { Text("Verify") }
                }
            } }
        }
    }
}

@Composable
private fun Setup(state: AppState, node: CovalentNodeClient, store: SecureNodeStore, scope: kotlinx.coroutines.CoroutineScope, modifier: Modifier) {
    var name by remember { mutableStateOf(store.displayName) }
    var address by remember { mutableStateOf(store.baseUrl) }
    var token by remember { mutableStateOf(store.token) }
    FormPage(modifier, "Connect your local node", "Covalent talks directly to a node you control. The bearer token stays encrypted on this Android device.") {
        OutlinedTextField(name, { name = it }, label = { Text("This device name") }, singleLine = true, modifier = Modifier.fillMaxWidth())
        OutlinedTextField(address, { address = it }, label = { Text("Node address") }, placeholder = { Text("http://192.168.1.20:8787") }, singleLine = true, modifier = Modifier.fillMaxWidth())
        OutlinedTextField(token, { token = it }, label = { Text("Local node token") }, visualTransformation = PasswordVisualTransformation(), singleLine = true, modifier = Modifier.fillMaxWidth())
        Text("Use HTTPS for a network node. Plain HTTP is accepted only on this device's loopback address, so your API token is never sent across a cleartext network.", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        FilledTonalButton(enabled = !state.busy, onClick = {
            api(state, scope) {
                val validated = validateAndPersistSetup(node, store, name, address, token)
                state.status = validated.status
                state.backups = validated.backups
                state.screen = Screen.HOME
            }
        }) { Text(if (state.busy) "Checking…" else "Connect") }
    }
}

@Composable
private fun Pair(state: AppState, node: CovalentNodeClient, store: SecureNodeStore, scope: kotlinx.coroutines.CoroutineScope, modifier: Modifier) {
    val focusManager = LocalFocusManager.current
    var invitation by remember { mutableStateOf("") }
    var responderName by remember { mutableStateOf(store.displayName.ifBlank { "Android" }) }
    FormPage(modifier, "Pair a device", "Discovery is only a reachability hint. Compare the four-part security code on both devices before trusting either one.") {
        Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            OutlinedButton(enabled = !state.busy, onClick = { api(state, scope) { state.discovered = node.discovery(store.baseUrl, store.token) } }) { Text("Find devices") }
            OutlinedButton(enabled = !state.busy, onClick = { api(state, scope) { state.providers = node.providers(store.baseUrl, store.token) } }) { Text("Refresh trusted") }
        }
        state.discovered.forEach { candidate -> AssistChip(onClick = { state.message = "Found ${candidate.endpoint}. Ask that device to share a pairing invitation; discovery alone never grants access." }, label = { Text("${candidate.source}: ${candidate.endpoint}") }) }
        OutlinedTextField(responderName, { responderName = it }, label = { Text("Your display name") }, modifier = Modifier.fillMaxWidth())
        OutlinedTextField(
            invitation,
            { invitation = it },
            label = { Text("Pairing invitation JSON") },
            minLines = 4,
            maxLines = 6,
            keyboardOptions = pairingInvitationKeyboardOptions,
            keyboardActions = KeyboardActions(onDone = { focusManager.clearFocus() }),
            modifier = Modifier.fillMaxWidth(),
        )
        if (state.pairingSession == null) FilledTonalButton(enabled = invitation.isNotBlank() && !state.busy, onClick = {
            api(state, scope) {
                state.pairingSession = node.post(store.baseUrl, store.token, "/api/v1/pair/accept", JSONObject()
                    .put("invitation", JSONObject(invitation)).put("responderName", responderName)
                    .put("responderRoles", JSONArray().put("backup_writer")).put("inviterRoles", JSONArray().put("storage_provider")))
            }
        }) { Text("Show security code") }
        state.pairingSession?.let { session ->
            SecurityConfirmation(session.getString("authenticationString"), onConfirm = {
                api(state, scope) {
                    val confirmed = node.post(store.baseUrl, store.token, "/api/v1/pair/confirm/responder", JSONObject()
                        .put("session", session).put("displayedCode", session.getString("authenticationString")))
                    state.pairingSession = confirmed
                    state.message = "Responder confirmation sent. Complete inviter confirmation on the other device, then exchange the updated signed session to finalize."
                }
            })
        }
    }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun Backup(state: AppState, node: CovalentNodeClient, store: SecureNodeStore, scope: kotlinx.coroutines.CoroutineScope, modifier: Modifier, pickSource: () -> Unit) {
    val context = androidx.compose.ui.platform.LocalContext.current
    var name by remember { mutableStateOf("") }
    var selected by remember { mutableStateOf(setOf<String>()) }
    LaunchedEffect(Unit) { if (state.providers.isEmpty()) api(state, scope) { state.providers = node.providers(store.baseUrl, store.token) } }
    FormPage(modifier, "Create a backup", "Pick a folder, then explicitly choose each device that may store an extra encrypted copy.") {
        OutlinedButton(onClick = pickSource) { Icon(Icons.Rounded.FolderOpen, null); Text(" Choose source folder") }
        Text(state.selectedSource?.let { DocumentFile.fromTreeUri(context, it)?.name ?: it.toString() } ?: "No folder selected")
        OutlinedTextField(name, { name = it }, label = { Text("Backup name") }, modifier = Modifier.fillMaxWidth())
        Text("Extra copies", style = MaterialTheme.typography.titleMedium)
        if (state.providers.isEmpty()) Text("No connected providers. This backup will remain local unless you pair and connect a device.", color = MaterialTheme.colorScheme.onSurfaceVariant)
        FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            state.providers.forEach { provider ->
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Checkbox(provider.peerId in selected, { checked -> selected = if (checked) selected + provider.peerId else selected - provider.peerId })
                    Text(provider.address, style = MaterialTheme.typography.bodyMedium)
                }
            }
        }
        FilledTonalButton(enabled = state.selectedSource != null && name.isNotBlank() && !state.busy, onClick = {
            val source = state.selectedSource ?: return@FilledTonalButton
            api(state, scope) {
                ensureReadableSource(context, source)
                val jobId = newId("backup")
                val snapshotId = newId("snapshot")
                val payload = JSONObject().put("displayName", name).put("snapshotId", snapshotId).put("jobId", jobId)
                    .put("selectedProviderIds", JSONArray(selected.toList()))
                queueTransfer(
                    context,
                    store,
                    jobId,
                    "/api/v1/backups/archive",
                    payload,
                    TransferWorker.MODE_SAF_BACKUP,
                    source,
                )
                state.message = "Backup queued. Android will stream the selected folder through protected file descriptors; its content URI never leaves this device."
                state.screen = Screen.HOME
            }
        }) { Icon(Icons.Rounded.Backup, null); Text(" Queue backup") }
    }
}

@Composable
private fun Restore(state: AppState, node: CovalentNodeClient, store: SecureNodeStore, scope: kotlinx.coroutines.CoroutineScope, modifier: Modifier, pickTarget: () -> Unit) {
    val context = androidx.compose.ui.platform.LocalContext.current
    var backupId by remember { mutableStateOf(state.backups.firstOrNull()?.backupId.orEmpty()) }
    var snapshotId by remember { mutableStateOf(state.backups.firstNotNullOfOrNull { it.latestSnapshotId }.orEmpty()) }
    FormPage(modifier, "Restore safely", "Covalent asks the node for a signed no-write preview before any restore work is queued.") {
        OutlinedTextField(backupId, { backupId = it }, label = { Text("Backup ID") }, modifier = Modifier.fillMaxWidth())
        OutlinedTextField(snapshotId, { snapshotId = it }, label = { Text("Snapshot ID") }, modifier = Modifier.fillMaxWidth())
        OutlinedButton(onClick = pickTarget) { Icon(Icons.Rounded.FolderOpen, null); Text(" Choose restore folder") }
        Text(state.selectedTarget?.toString() ?: "No authorized target selected")
        OutlinedButton(enabled = state.selectedTarget != null && backupId.isNotBlank() && snapshotId.isNotBlank() && !state.busy, onClick = {
            val target = state.selectedTarget ?: return@OutlinedButton
            api(state, scope) {
                ensureWritableTarget(context, target)
                state.restorePlan = node.post(store.baseUrl, store.token, "/api/v1/restores/archive/preview", JSONObject()
                    .put("backupId", backupId).put("snapshotId", snapshotId)
                    .put("conflictPolicy", "fail").put("jobId", newId("restore")))
            }
        }) { Text("Preview restore") }
        state.restorePlan?.let { plan ->
            val entries = plan.getJSONArray("entries")
            StatusCard("Signed preview", "${entries.length()} changes", "The node signed these exact contents. Android confines writes to your authorized empty folder.", Icons.Rounded.CheckCircle)
            FilledTonalButton(enabled = !state.busy, onClick = {
                val target = state.selectedTarget ?: return@FilledTonalButton
                api(state, scope) {
                    queueTransfer(
                        context,
                        store,
                        plan.getString("jobId"),
                        "/api/v1/restores/archive/execute",
                        JSONObject().put("plan", plan),
                        TransferWorker.MODE_SAF_RESTORE,
                        target,
                    )
                    state.message = "Restore queued from the signed preview. Android will stream files into the authorized folder through protected file descriptors."
                    state.screen = Screen.HOME
                }
            }) { Icon(Icons.Rounded.FolderOpen, null); Text(" Queue restore") }
        }
    }
}

@Composable
private fun Settings(state: AppState, node: CovalentNodeClient, store: SecureNodeStore, scope: kotlinx.coroutines.CoroutineScope, modifier: Modifier, export: (String) -> Unit, import: (Array<String>) -> Unit, onLanChange: (Boolean) -> Unit) {
    var lanEnabled by remember { mutableStateOf(state.status?.lanDiscovery ?: false) }
    FormPage(modifier, "Settings", "Settings export includes your device name and remembered backups, never private identity keys or provider credentials.") {
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f)) { Text("LAN discovery", style = MaterialTheme.typography.titleMedium); Text("Show nearby node hints. Pairing still requires code confirmation.", color = MaterialTheme.colorScheme.onSurfaceVariant) }
            Switch(lanEnabled, onCheckedChange = { enabled -> lanEnabled = enabled; onLanChange(enabled) })
        }
        OutlinedButton(enabled = !state.busy, onClick = { export("covalent-settings.json") }) { Text("Export safe settings") }
        OutlinedButton(enabled = !state.busy, onClick = { import(arrayOf("application/json", "text/json")) }) { Text("Import settings") }
        OutlinedButton(onClick = { state.screen = Screen.SETUP }) { Text("Change node connection") }
        Text("If folder access is revoked in Android system settings, a backup or restore stops safely and asks you to choose the folder again.", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
}

@Composable
private fun SecurityConfirmation(code: String, onConfirm: () -> Unit) {
    Card(Modifier.fillMaxWidth()) { Column(Modifier.padding(18.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
        Text("Compare this code on both devices", style = MaterialTheme.typography.titleMedium)
        Text(code, style = MaterialTheme.typography.headlineSmall, fontWeight = FontWeight.Bold)
        Text("Only confirm when every group matches. This does not trust a discovered device automatically.")
        FilledTonalButton(onClick = onConfirm) { Icon(Icons.Rounded.Security, null); Text(" Codes match") }
    } }
}

@Composable
private fun FormPage(modifier: Modifier, title: String, subtitle: String, content: @Composable () -> Unit) {
    LazyColumn(modifier, contentPadding = PaddingValues(20.dp, 14.dp, 20.dp, 32.dp), verticalArrangement = Arrangement.spacedBy(14.dp)) {
        item { Text(title, style = MaterialTheme.typography.headlineMedium, fontWeight = FontWeight.SemiBold) }
        item { Text(subtitle, color = MaterialTheme.colorScheme.onSurfaceVariant) }
        item { Column(verticalArrangement = Arrangement.spacedBy(14.dp), content = { content() }) }
    }
}

@Composable
private fun StatusCard(title: String, value: String, detail: String, icon: androidx.compose.ui.graphics.vector.ImageVector) {
    Card(Modifier.fillMaxWidth()) { Row(Modifier.padding(18.dp), horizontalArrangement = Arrangement.spacedBy(14.dp), verticalAlignment = Alignment.Top) {
        Surface(shape = MaterialTheme.shapes.medium, color = MaterialTheme.colorScheme.secondaryContainer) { Icon(icon, null, Modifier.padding(12.dp)) }
        Column { Text(title, style = MaterialTheme.typography.labelLarge); Text(value, style = MaterialTheme.typography.titleLarge, fontWeight = FontWeight.SemiBold); Spacer(Modifier.height(5.dp)); Text(detail, color = MaterialTheme.colorScheme.onSurfaceVariant) }
    } }
}

@Composable
private fun EmptyState(title: String, detail: String) { Card(Modifier.fillMaxWidth()) { Row(Modifier.padding(18.dp), horizontalArrangement = Arrangement.spacedBy(14.dp)) { Icon(Icons.Rounded.CloudOff, null); Column { Text(title, fontWeight = FontWeight.SemiBold); Text(detail, color = MaterialTheme.colorScheme.onSurfaceVariant) } } } }

@Composable
fun PrimaryActionToolbar(enabled: Boolean, onAction: (PrimaryAction) -> Unit) {
    Surface(modifier = Modifier.semantics { contentDescription = "Primary actions" }.padding(horizontal = 12.dp), shape = MaterialTheme.shapes.extraLarge, color = MaterialTheme.colorScheme.surfaceContainerHigh, tonalElevation = 6.dp, shadowElevation = 3.dp) {
        Row(Modifier.padding(8.dp), horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
            ToolbarButton(PrimaryAction.PAIR, enabled, onAction, Icons.Rounded.AddLink)
            ToolbarButton(PrimaryAction.BACKUP, enabled, onAction, Icons.Rounded.Backup)
            ToolbarButton(PrimaryAction.RESTORE, enabled, onAction, Icons.Rounded.FolderOpen)
        }
    }
}

@Composable
private fun ToolbarButton(action: PrimaryAction, enabled: Boolean, onAction: (PrimaryAction) -> Unit, icon: androidx.compose.ui.graphics.vector.ImageVector) {
    FilledTonalButton(onClick = { onAction(action) }, enabled = enabled, contentPadding = PaddingValues(horizontal = 14.dp, vertical = 12.dp)) { Icon(icon, null); Text(action.label, Modifier.padding(start = 7.dp)) }
}

private fun api(state: AppState, scope: kotlinx.coroutines.CoroutineScope, work: suspend () -> Unit) {
    scope.launch {
        state.busy = true
        runCatching { withContext(Dispatchers.IO) { work() } }.onFailure { state.message = it.message ?: "The node could not complete that action." }
        state.busy = false
    }
}

private fun refreshStatus(state: AppState, node: CovalentNodeClient, store: SecureNodeStore) {
    state.status = node.status(store.baseUrl)
    if (store.token.isNotBlank()) {
        val backups = node.backups(store.baseUrl, store.token)
        store.replaceBackups(backups)
        state.backups = backups
    }
}

private fun setLanDiscovery(state: AppState, node: CovalentNodeClient, store: SecureNodeStore, enabled: Boolean) {
    val settings = node.post(store.baseUrl, store.token, "/api/v1/config/export", JSONObject())
    settings.put("lanDiscoveryEnabled", enabled)
    node.postNoContent(store.baseUrl, store.token, "/api/v1/config/import", JSONObject().put("confirmed", true).put("settings", settings))
    refreshStatus(state, node, store)
    state.message = if (enabled) "LAN discovery enabled after local-network permission and explicit settings confirmation." else "LAN discovery disabled. Manual and Tailnet pairing remain available."
}

private fun ensureReadableSource(context: android.content.Context, uri: Uri) {
    check(context.contentResolver.persistedUriPermissions.any { it.uri == uri && it.isReadPermission }) { "Folder access was revoked. Choose the source folder again." }
}

private fun ensureWritableTarget(context: android.content.Context, uri: Uri) {
    check(context.contentResolver.persistedUriPermissions.any { it.uri == uri && it.isReadPermission && it.isWritePermission }) { "Folder write access was revoked. Choose the restore folder again." }
    val target = DocumentFile.fromTreeUri(context, uri)
    check(target != null && target.exists() && target.isDirectory) { "The selected restore folder is unavailable." }
    check(target.listFiles().isEmpty()) { "Choose an empty restore folder so the signed no-write preview remains exact." }
}

private fun queueTransfer(
    context: android.content.Context,
    store: SecureNodeStore,
    jobId: String,
    path: String,
    payload: JSONObject,
    mode: String = "json",
    treeUri: Uri? = null,
) {
    store.savePending(jobId, path, payload, mode, treeUri?.toString())
    TransferScheduler.enqueue(context, jobId)
}

@Preview(showBackground = true, widthDp = 420, heightDp = 820)
@Composable
private fun AppPreview() { CovalentTheme { CovalentApp() } }
