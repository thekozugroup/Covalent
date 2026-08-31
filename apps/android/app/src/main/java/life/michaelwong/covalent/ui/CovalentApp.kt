package life.michaelwong.covalent.ui

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.text.format.DateUtils
import android.util.Log
import android.text.format.Formatter
import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
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
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.rounded.ArrowBack
import androidx.compose.material.icons.rounded.AddLink
import androidx.compose.material.icons.rounded.Backup
import androidx.compose.material.icons.rounded.Cancel
import androidx.compose.material.icons.rounded.CheckCircle
import androidx.compose.material.icons.rounded.CloudOff
import androidx.compose.material.icons.rounded.ContentCopy
import androidx.compose.material.icons.rounded.ErrorOutline
import androidx.compose.material.icons.rounded.FolderOpen
import androidx.compose.material.icons.rounded.Pause
import androidx.compose.material.icons.rounded.PlayArrow
import androidx.compose.material.icons.rounded.Refresh
import androidx.compose.material.icons.rounded.Search
import androidx.compose.material.icons.rounded.Security
import androidx.compose.material.icons.rounded.Settings
import androidx.compose.material.icons.rounded.Storage
import androidx.compose.material3.AssistChip
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Card
import androidx.compose.material3.Checkbox
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedCard
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Surface
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalFocusManager
import androidx.compose.ui.platform.LocalResources
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import androidx.documentfile.provider.DocumentFile
import androidx.lifecycle.viewmodel.compose.viewModel
import java.io.InputStream
import java.net.URI
import java.net.InetAddress
import java.net.URLDecoder
import java.nio.charset.StandardCharsets
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlin.math.roundToLong
import life.michaelwong.covalent.R
import life.michaelwong.covalent.data.CovalentNodeClient
import life.michaelwong.covalent.data.EnrolledTrust
import life.michaelwong.covalent.data.NodeApiException
import life.michaelwong.covalent.data.SafTransferBridge
import life.michaelwong.covalent.data.SecureNodeStore
import life.michaelwong.covalent.data.encodeEnrolledCertificate
import life.michaelwong.covalent.data.newId
import life.michaelwong.covalent.data.normalizeSha256Pin
import life.michaelwong.covalent.data.restoreTransferPayload
import life.michaelwong.covalent.data.isSafeSafRestoreAction
import life.michaelwong.covalent.model.DiscoveryCandidate
import life.michaelwong.covalent.model.NodeStatus
import life.michaelwong.covalent.model.NetworkPairing
import life.michaelwong.covalent.model.NetworkPairingDirection
import life.michaelwong.covalent.model.NetworkPairingState
import life.michaelwong.covalent.model.NodeConnection
import life.michaelwong.covalent.model.PrimaryAction
import life.michaelwong.covalent.model.PeerTransport
import life.michaelwong.covalent.model.Provider
import life.michaelwong.covalent.model.ProviderReachability
import life.michaelwong.covalent.model.RememberedBackup
import life.michaelwong.covalent.model.RestorePlanPage
import life.michaelwong.covalent.model.RestoreConflictPolicy
import life.michaelwong.covalent.model.TransferKind
import life.michaelwong.covalent.model.TransferRecord
import life.michaelwong.covalent.model.TransferState
import life.michaelwong.covalent.ui.theme.CovalentTheme
import life.michaelwong.covalent.node.ActiveNodeConnectionResolver
import life.michaelwong.covalent.node.EmbeddedNodeManager
import life.michaelwong.covalent.node.EmbeddedProviderState
import life.michaelwong.covalent.node.KeyProtectionLevel
import life.michaelwong.covalent.node.NodeMode
import life.michaelwong.covalent.work.TransferScheduler
import life.michaelwong.covalent.work.TransferExecution
import life.michaelwong.covalent.work.TransferWorker
import org.json.JSONArray
import org.json.JSONObject

private const val UI_LOG_TAG = "CovalentUi"

internal enum class Screen { HOME, SETUP, PAIR, BACKUP, RESTORE, SETTINGS }

internal fun Screen.systemBackTarget(): Screen? = when (this) {
    Screen.PAIR, Screen.BACKUP, Screen.RESTORE, Screen.SETTINGS -> Screen.HOME
    Screen.HOME, Screen.SETUP -> null
}

internal val pairingInvitationKeyboardOptions = KeyboardOptions(
    capitalization = KeyboardCapitalization.None,
    autoCorrectEnabled = false,
    keyboardType = KeyboardType.Password,
    imeAction = ImeAction.Done,
)

internal data class ValidatedSetup(
    val status: NodeStatus,
    val backups: List<RememberedBackup>,
)

private const val CLAIM_TOKEN_FILE_MAX_BYTES = 1_024

internal fun parseClaimTokenFile(bytes: ByteArray): String {
    require(bytes.size <= CLAIM_TOKEN_FILE_MAX_BYTES) { "token file is too large" }
    val token = bytes.toString(Charsets.UTF_8).trim()
    require(token.length in 32..512) { "token length is invalid" }
    require(token.all { it.code in 0x21..0x7e && it != '"' && it != '\\' }) {
        "token contains invalid characters"
    }
    return token
}

internal fun readClaimTokenFile(input: InputStream): String {
    val bounded = ByteArray(CLAIM_TOKEN_FILE_MAX_BYTES + 1)
    var count = 0
    while (count < bounded.size) {
        val read = input.read(bounded, count, bounded.size - count)
        if (read < 0) break
        if (read == 0) continue
        count += read
    }
    require(count <= CLAIM_TOKEN_FILE_MAX_BYTES) { "token file is too large" }
    return parseClaimTokenFile(bounded.copyOf(count))
}

internal fun validateAndPersistSetup(
    node: CovalentNodeClient,
    store: SecureNodeStore,
    displayName: String,
    address: String,
    token: String,
    enrolledTrust: EnrolledTrust? = null,
): ValidatedSetup {
    val normalizedAddress = normalizeEndpointInput(address)
    val status = node.status(normalizedAddress)
    val backups = node.backups(normalizedAddress, token)
    store.displayName = displayName
    store.baseUrl = normalizedAddress
    store.token = token
    store.saveEnrolledTrust(enrolledTrust)
    store.replaceBackups(backups)
    return ValidatedSetup(status, backups)
}

internal fun shouldReturnToSetupAfterRefreshFailure(error: Throwable): Boolean =
    error is NodeApiException && error.statusCode == 401

internal fun startupRefreshDispatcher(): CoroutineDispatcher = Dispatchers.IO

internal enum class AddressIssue { NONE, MISSING, MALFORMED, INSECURE_REMOTE }

internal fun validateNodeAddress(raw: String): AddressIssue {
    val value = normalizeEndpointInput(raw)
    if (value.isBlank()) return AddressIssue.MISSING
    val uri = runCatching { URI(value) }.getOrNull() ?: return AddressIssue.MALFORMED
    val scheme = uri.scheme?.lowercase()
    val host = uri.host?.lowercase()
    if (
        scheme !in setOf("http", "https") || host.isNullOrBlank() || uri.userInfo != null ||
        (uri.rawPath?.takeIf(String::isNotEmpty) ?: "/") != "/" || uri.rawQuery != null ||
        uri.rawFragment != null || (uri.port != -1 && uri.port !in 1..65_535)
    ) {
        return AddressIssue.MALFORMED
    }
    if (scheme == "http" && !isLoopbackHost(host)) return AddressIssue.INSECURE_REMOTE
    return AddressIssue.NONE
}

internal fun normalizeEndpointInput(raw: String): String {
    val trimmed = raw.trim().removeSuffix("/")
    if (!trimmed.startsWith("covalent://connect?")) return trimmed
    return trimmed.substringAfter('?').split('&')
        .mapNotNull { item ->
            val parts = item.split('=', limit = 2)
            if (parts.size == 2 && parts[0] == "endpoint") {
                URLDecoder.decode(parts[1], StandardCharsets.UTF_8.name())
            } else null
        }
        .firstOrNull()
        .orEmpty()
        .trim()
        .removeSuffix("/")
}

/**
 * Extracts a usable server address from a `covalent://connect?endpoint=…` link.
 *
 * Links arrive from other apps and are untrusted, so anything that is not a well-formed
 * Covalent setup link — or that carries an address Covalent would refuse to type by hand —
 * yields an empty string and is ignored. A link can only prefill the address field; the
 * access token and any certificate are still entered by the person setting the app up.
 */
internal fun setupLinkEndpoint(link: String): String {
    val trimmed = link.trim()
    if (!trimmed.startsWith("covalent://connect?")) return ""
    val endpoint = normalizeEndpointInput(trimmed)
    return if (validateNodeAddress(endpoint) == AddressIssue.NONE) endpoint else ""
}

internal sealed interface SetupLinkOutcome {
    /** The link is well formed and this app has no server yet, so it may prefill setup. */
    data class Apply(val endpoint: String) : SetupLinkOutcome

    /** The link is malformed, or names an address the setup form would refuse. */
    object Rejected : SetupLinkOutcome

    /** A server is already saved. An outside app must not redirect a configured install. */
    object AlreadyConnected : SetupLinkOutcome
}

/**
 * Decides what a `covalent://` link may do.
 *
 * Any app on the phone can send one of these links, so a link is never allowed to point an
 * already-configured install at a different server — that would put a familiar-looking
 * token field in front of an address the person never chose. Once a server is saved, the
 * only way to change it is Settings.
 */
internal fun setupLinkOutcome(link: String, hasSavedServer: Boolean): SetupLinkOutcome {
    val endpoint = setupLinkEndpoint(link)
    if (endpoint.isEmpty()) return SetupLinkOutcome.Rejected
    if (hasSavedServer) return SetupLinkOutcome.AlreadyConnected
    return SetupLinkOutcome.Apply(endpoint)
}

internal fun requiresLocalNetworkPermission(raw: String, sdkInt: Int): Boolean {
    if (sdkInt < 37) return false
    val host = runCatching { URI(normalizeEndpointInput(raw)).host?.lowercase() }.getOrNull() ?: return false
    if (isLoopbackHost(host)) return false
    return host.endsWith(".local") || '.' !in host || isPrivateIpv4(host) ||
        host.startsWith("fc") || host.startsWith("fd") || host.startsWith("fe80:")
}

private fun isPrivateIpv4(host: String): Boolean {
    val octets = host.split('.').mapNotNull(String::toIntOrNull)
    if (octets.size != 4 || octets.any { it !in 0..255 }) return false
    return octets[0] == 10 ||
        octets[0] == 127 ||
        octets[0] == 169 && octets[1] == 254 ||
        octets[0] == 192 && octets[1] == 168 ||
        octets[0] == 172 && octets[1] in 16..31 ||
        octets[0] == 100 && octets[1] in 64..127
}

private fun isLoopbackHost(host: String): Boolean =
    host == "localhost" || host == "127.0.0.1" || host == "::1" || host == "[::1]"

private const val LOCAL_NETWORK_PERMISSION = "android.permission.ACCESS_LOCAL_NETWORK"
private const val STATUS_POLL_MILLIS = 4_000L
private const val TRANSFER_POLL_MILLIS = 750L
private const val PAIRING_POLL_MILLIS = 2_000L
private val ALL_PAIRING_ROLES = listOf("storage_provider", "backup_reader", "backup_writer")
private const val GIBIBYTE = 1_073_741_824L

internal fun providerCapacityBytes(maximumGiB: String, keepFreeGiB: String): Pair<Long, Long>? {
    fun parse(value: String): Long? {
        val gibibytes = value.trim().toDoubleOrNull()?.takeIf { it.isFinite() && it >= 0.0 } ?: return null
        val bytes = gibibytes * GIBIBYTE.toDouble()
        return bytes.takeIf { it <= Long.MAX_VALUE.toDouble() }?.roundToLong()
    }
    val maximum = parse(maximumGiB) ?: return null
    val keepFree = parse(keepFreeGiB) ?: return null
    return (maximum to keepFree).takeIf {
        maximum >= GIBIBYTE / 2 && keepFree <= maximum - GIBIBYTE / 2
    }
}

private fun providerGiBText(bytes: Long): String {
    val halfGiB = GIBIBYTE / 2
    val halfSteps = ((bytes.coerceAtLeast(0L) + halfGiB / 2) / halfGiB)
    return if (halfSteps % 2L == 0L) (halfSteps / 2L).toString() else "${halfSteps / 2L}.5"
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun CovalentApp(
    storeOverride: SecureNodeStore? = null,
    stateOverride: CovalentViewModel? = null,
) {
    val context = LocalContext.current
    val resources = LocalResources.current
    val store = remember(storeOverride) { storeOverride ?: SecureNodeStore(context.applicationContext) }
    val state = stateOverride ?: viewModel()
    val embeddedManager = remember(context) { EmbeddedNodeManager(context.applicationContext) }
    val connectionResolver = remember(context) { ActiveNodeConnectionResolver(context.applicationContext) }
    val embeddedProvider by embeddedManager.state.collectAsState()
    val activeConnection = connectionResolver.activeConnection(store)
    val node = remember(store, state) { CovalentNodeClient(state::pendingEnrolledTrust) }
    val scope = rememberCoroutineScope()
    val snackbar = remember { SnackbarHostState() }

    LaunchedEffect(embeddedProvider.maxBytes, embeddedProvider.keepFreeBytes, embeddedProvider.lanDiscoveryRequested) {
        state.loadProviderDraft(
            providerGiBText(embeddedProvider.maxBytes),
            providerGiBText(embeddedProvider.keepFreeBytes),
            embeddedProvider.lanDiscoveryRequested,
        )
    }

    fun submitSetup() {
        api(context, state, scope, onError = { error ->
            state.setupConnectionError =
                nodeFailureMessage(context, error, R.string.error_connection_failed)
        }) {
            val validated = validateAndPersistSetup(
                node,
                store,
                state.setupName.trim(),
                state.setupAddress,
                state.setupToken,
                state.pendingEnrolledTrust(),
            )
            embeddedManager.selectExternalMode()
            state.status = validated.status
            state.backups = validated.backups
            state.connectionHealth = ConnectionHealth.READY
            state.connectionError = null
            state.lastConnectedAtUnixMs = System.currentTimeMillis()
            state.setupConnectionError = ""
            state.screen = Screen.HOME
        }
    }

    fun setLan(enabled: Boolean) {
        val connection = activeConnection ?: return
        api(context, state, scope) {
            setLanDiscovery(context, state, node, store, connection, enabled)
        }
    }

    fun discover() {
        val connection = activeConnection ?: return
        state.discoveryRunning = true
        state.discoveryCompleted = false
        state.discoveryError = null
        api(context, state, scope, onError = { error ->
            state.discoveryRunning = false
            state.discoveryCompleted = true
            state.discoveryError = nodeFailureMessage(context, error, R.string.discovery_error)
        }) {
            state.discovered = node.discovery(connection.baseUrl, connection.token)
            state.discoveryRunning = false
            state.discoveryCompleted = true
        }
    }

    val providerPermissions = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestMultiplePermissions(),
    ) { grants ->
        if (!state.pendingProviderEnable) return@rememberLauncherForActivityResult
        state.pendingProviderEnable = false
        if (grants.values.all { it }) {
            providerCapacityBytes(state.providerMaximumGiB, state.providerKeepFreeGiB)?.let { (maximum, keepFree) ->
                embeddedManager.enable(maximum, keepFree, state.providerLanDiscovery)
            }
        } else {
            state.notice = resources.getString(R.string.phone_provider_permission_denied)
        }
    }

    fun requestProviderEnable() {
        val capacity = providerCapacityBytes(state.providerMaximumGiB, state.providerKeepFreeGiB)
        if (capacity == null) {
            state.notice = resources.getString(R.string.phone_provider_invalid_capacity)
            return
        }
        if (!embeddedProvider.supported || !embeddedProvider.keyProtectionAvailable) {
            state.notice = resources.getString(R.string.phone_provider_locked)
            return
        }
        val missing = buildList {
            if (
                Build.VERSION.SDK_INT >= 33 && ContextCompat.checkSelfPermission(
                    context,
                    android.Manifest.permission.POST_NOTIFICATIONS,
                ) != PackageManager.PERMISSION_GRANTED
            ) {
                add(android.Manifest.permission.POST_NOTIFICATIONS)
            }
            if (
                Build.VERSION.SDK_INT >= 37 &&
                ContextCompat.checkSelfPermission(context, LOCAL_NETWORK_PERMISSION) != PackageManager.PERMISSION_GRANTED
            ) {
                add(LOCAL_NETWORK_PERMISSION)
            }
        }
        if (missing.isEmpty()) {
            embeddedManager.enable(capacity.first, capacity.second, state.providerLanDiscovery)
        } else {
            state.pendingProviderEnable = true
            providerPermissions.launch(missing.toTypedArray())
        }
    }

    val localNetworkPermission = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission(),
    ) { granted ->
        when {
            granted && state.pendingPermissionSetup -> {
                state.pendingPermissionSetup = false
                state.localPermissionDenied = false
                submitSetup()
            }
            granted && state.pendingPermissionLanEnable -> {
                state.pendingPermissionLanEnable = false
                state.localPermissionDenied = false
                setLan(true)
            }
            granted && state.pendingPermissionDiscovery -> {
                state.pendingPermissionDiscovery = false
                state.localPermissionDenied = false
                discover()
            }
            else -> {
                val deniedSetup = state.pendingPermissionSetup
                val deniedDiscovery = state.pendingPermissionDiscovery
                state.pendingPermissionSetup = false
                state.pendingPermissionLanEnable = false
                state.pendingPermissionDiscovery = false
                state.localPermissionDenied = true
                if (deniedSetup) {
                    state.setupConnectionError = resources.getString(R.string.lan_permission_denied)
                }
                if (deniedDiscovery) {
                    state.discoveryRunning = false
                    state.discoveryCompleted = true
                    state.discoveryError = resources.getString(R.string.lan_permission_denied)
                }
                state.notice = resources.getString(R.string.lan_permission_denied)
            }
        }
    }

    fun requestSetup() {
        state.setupNameError = if (state.setupName.trim().isEmpty()) {
            resources.getString(R.string.error_device_name_required)
        } else ""
        state.setupAddressError = when (validateNodeAddress(state.setupAddress)) {
            AddressIssue.NONE -> ""
            AddressIssue.MISSING -> resources.getString(R.string.error_node_address_required)
            AddressIssue.MALFORMED -> resources.getString(R.string.error_node_address_invalid)
            AddressIssue.INSECURE_REMOTE -> resources.getString(R.string.error_node_address_insecure)
        }
        state.setupTokenError = if (state.setupToken.isBlank()) {
            resources.getString(R.string.error_node_token_required)
        } else ""
        state.setupCertificatePinError = if (runCatching {
            if (state.setupCertificatePin.isNotBlank()) normalizeSha256Pin(state.setupCertificatePin)
        }.isFailure) resources.getString(R.string.error_certificate_pin_invalid) else ""
        if (state.setupNameError.isNotEmpty() || state.setupAddressError.isNotEmpty() || state.setupTokenError.isNotEmpty()) {
            return
        }
        if (state.setupCertificatePinError.isNotEmpty()) return
        if (
            requiresLocalNetworkPermission(state.setupAddress, Build.VERSION.SDK_INT) &&
            ContextCompat.checkSelfPermission(context, LOCAL_NETWORK_PERMISSION) != PackageManager.PERMISSION_GRANTED
        ) {
            state.pendingPermissionSetup = true
            localNetworkPermission.launch(LOCAL_NETWORK_PERMISSION)
        } else {
            submitSetup()
        }
    }

    val sourcePicker = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocumentTree()) { uri ->
        uri?.let {
            context.contentResolver.takePersistableUriPermission(it, Intent.FLAG_GRANT_READ_URI_PERMISSION)
            state.selectedSource = it
            state.notice = resources.getString(R.string.source_access_saved)
        }
    }
    val caCertificatePicker = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
        if (uri == null) return@rememberLauncherForActivityResult
        runCatching {
            val bytes = context.contentResolver.openInputStream(uri)?.use { it.readBytes() }
                ?: error(resources.getString(R.string.error_file_not_readable))
            state.setupCaCertificateDer = encodeEnrolledCertificate(bytes)
            state.setupCaCertificateLabel = DocumentFile.fromSingleUri(context, uri)?.name ?: "saved"
            state.setupCertificatePin = ""
            state.setupCertificatePinError = ""
        }.onFailure {
            Log.w(UI_LOG_TAG, "CA certificate enrolment failed", it)
            state.setupConnectionError = resources.getString(R.string.error_ca_certificate_invalid)
        }
    }
    val tokenPicker = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
        if (uri == null) return@rememberLauncherForActivityResult
        runCatching {
            val document = DocumentFile.fromSingleUri(context, uri)
            check(document?.isFile == true)
            val declaredLength = document.length()
            check(declaredLength <= CLAIM_TOKEN_FILE_MAX_BYTES || declaredLength == 0L)
            val parsed = context.contentResolver.openInputStream(uri)?.use(::readClaimTokenFile)
                ?: error(resources.getString(R.string.error_file_not_readable))
            state.setupToken = parsed
            state.setupTokenError = ""
        }.onFailure {
            Log.w(UI_LOG_TAG, "Access token file rejected")
            state.setupTokenError = resources.getString(R.string.error_token_file_invalid)
        }
    }
    val targetPicker = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocumentTree()) { uri ->
        uri?.let {
            context.contentResolver.takePersistableUriPermission(
                it,
                Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION,
            )
            state.selectedTarget = it
            state.persistRestorePlan(null)
        }
    }
    val createSettings = rememberLauncherForActivityResult(
        ActivityResultContracts.CreateDocument("application/json"),
    ) { uri ->
        if (uri == null) return@rememberLauncherForActivityResult
        val connection = activeConnection ?: return@rememberLauncherForActivityResult
        api(context, state, scope) {
            val exported = node.post(connection.baseUrl, connection.token, "/api/v1/config/export", JSONObject())
            context.contentResolver.openOutputStream(uri)?.bufferedWriter()?.use { it.write(exported.toString(2)) }
                ?: error(resources.getString(R.string.error_file_not_writable))
            state.notice = resources.getString(R.string.settings_exported)
        }
    }
    val importSettings = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
        if (uri == null) return@rememberLauncherForActivityResult
        val connection = activeConnection ?: return@rememberLauncherForActivityResult
        api(context, state, scope) {
            val text = context.contentResolver.openInputStream(uri)?.bufferedReader()?.use { it.readText() }
                ?: error(resources.getString(R.string.error_file_not_readable))
            check(text.encodeToByteArray().size <= 4 * 1_024 * 1_024) {
                resources.getString(R.string.error_settings_file_too_large)
            }
            val candidate = JSONObject(text)
            val current = node.post(connection.baseUrl, connection.token, "/api/v1/config/export", JSONObject())
            validateSettingsCandidate(context, candidate)
            state.setImportCandidate(candidate, current)
        }
    }

    LaunchedEffect(store, state) {
        state.initialize(store)
    }
    LaunchedEffect(state.pendingSetupLink) {
        val link = state.pendingSetupLink
        if (link.isBlank()) return@LaunchedEffect
        state.pendingSetupLink = ""
        when (val outcome = setupLinkOutcome(link, store.baseUrl.isNotBlank())) {
            is SetupLinkOutcome.Apply -> {
                state.setupAddress = outcome.endpoint
                state.setupAddressError = ""
                state.setupConnectionError = ""
                state.screen = Screen.SETUP
                state.notice = resources.getString(R.string.setup_link_applied)
            }
            SetupLinkOutcome.AlreadyConnected ->
                state.notice = resources.getString(R.string.setup_link_ignored)
            SetupLinkOutcome.Rejected ->
                state.notice = resources.getString(R.string.setup_link_rejected)
        }
    }
    LaunchedEffect(state.screen, activeConnection?.baseUrl, activeConnection?.token) {
        val connection = activeConnection ?: run {
            state.connectionHealth = ConnectionHealth.DISCONNECTED
            return@LaunchedEffect
        }
        while (isActive) {
            if (state.connectionHealth != ConnectionHealth.READY) {
                state.connectionHealth = ConnectionHealth.CONNECTING
            }
            runCatching {
                withContext(startupRefreshDispatcher()) {
                    refreshStatus(state, node, store, connection)
                    TransferExecution.reconcileAcknowledgements(store, node, connection)
                }
            }.onSuccess {
                state.connectionHealth = ConnectionHealth.READY
                state.connectionError = null
                state.lastConnectedAtUnixMs = System.currentTimeMillis()
            }.onFailure { error ->
                state.connectionHealth = ConnectionHealth.STALE
                state.connectionError = nodeFailureMessage(context, error, R.string.error_connection_failed)
                if (shouldReturnToSetupAfterRefreshFailure(error)) state.screen = Screen.SETUP
            }
            delay(STATUS_POLL_MILLIS)
        }
    }
    LaunchedEffect(activeConnection?.baseUrl, activeConnection?.token) {
        val connection = activeConnection
        if (connection != null) runCatching {
            withContext(Dispatchers.IO) {
                TransferExecution.reconcileAcknowledgements(
                    store,
                    CovalentNodeClient(store::enrolledTrust),
                    connection,
                )
                TransferScheduler.requeuePending(context, store)
            }
        }.onFailure {
            state.notice = nodeFailureMessage(context, it, R.string.error_resume_transfer)
        }
        while (isActive) {
            state.refreshDurableState()
            delay(TRANSFER_POLL_MILLIS)
        }
    }
    LaunchedEffect(activeConnection?.baseUrl, activeConnection?.token) {
        val connection = activeConnection
        while (isActive) {
            if (connection != null) {
                runCatching {
                    withContext(Dispatchers.IO) {
                        node.pendingNetworkPairings(connection.baseUrl, connection.token)
                    }
                }.onSuccess { pending ->
                    state.networkPairings = pending.sortedBy(NetworkPairing::expiresAtUnixMs)
                    val active = state.activeNetworkPairing
                    val updated = active?.let { current ->
                        pending.firstOrNull { it.pairingId == current.pairingId }
                    }
                    when {
                        updated != null -> state.activeNetworkPairing = updated
                        active != null -> state.activeNetworkPairing = null
                        else -> state.activeNetworkPairing = pending.firstOrNull {
                            it.direction == NetworkPairingDirection.INCOMING &&
                                it.state != NetworkPairingState.FAILED
                        }
                    }
                    if (state.activeNetworkPairing != null && state.screen != Screen.SETUP) {
                        state.screen = Screen.PAIR
                    }
                }
                state.activeNetworkPairing
                    ?.takeIf { it.state == NetworkPairingState.COMPLETE }
                    ?.let { completed ->
                        runCatching {
                            withContext(Dispatchers.IO) {
                                completeNetworkPairingConnection(state, node, connection, completed)
                            }
                        }.onFailure { error ->
                            state.pairingError =
                                nodeFailureMessage(context, error, R.string.error_pairing_failed)
                        }
                    }
            }
            delay(PAIRING_POLL_MILLIS)
        }
    }
    LaunchedEffect(state.notice) {
        val notice = state.notice ?: return@LaunchedEffect
        snackbar.showSnackbar(notice)
        state.notice = null
    }
    BackHandler(enabled = state.screen.systemBackTarget() != null) {
        state.screen.systemBackTarget()?.let { state.screen = it }
    }

    val compactActions = LocalDensity.current.fontScale >= 1.3f
    Scaffold(
        snackbarHost = { SnackbarHost(snackbar) },
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.app_name)) },
                navigationIcon = if (state.screen != Screen.HOME && state.screen != Screen.SETUP) {
                    {
                        IconButton(onClick = { state.screen = Screen.HOME }) {
                            Icon(
                                Icons.AutoMirrored.Rounded.ArrowBack,
                                stringResource(R.string.action_back),
                            )
                        }
                    }
                } else ({}) ,
                actions = {
                    if (state.screen == Screen.HOME) {
                        IconButton(onClick = { state.screen = Screen.SETTINGS }) {
                            Icon(Icons.Rounded.Settings, stringResource(R.string.action_settings))
                        }
                    }
                },
                colors = TopAppBarDefaults.topAppBarColors(containerColor = MaterialTheme.colorScheme.surface),
            )
        },
        floatingActionButton = {
            if (state.screen == Screen.HOME) {
                PrimaryActionToolbar(
                    enabled = state.connectionHealth == ConnectionHealth.READY,
                    compact = compactActions,
                ) { action ->
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
                Screen.HOME -> Home(state, node, store, activeConnection, scope, page)
                Screen.SETUP -> Setup(
                    state,
                    page,
                    ::requestSetup,
                    pickTokenFile = { tokenPicker.launch(arrayOf("text/plain", "application/octet-stream")) },
                    pickCaCertificate = { caCertificatePicker.launch(arrayOf("application/x-x509-ca-cert", "application/pkix-cert", "text/plain")) },
                )
                Screen.PAIR -> Pair(state, node, activeConnection, scope, page) {
                    if (
                        Build.VERSION.SDK_INT >= 37 &&
                        ContextCompat.checkSelfPermission(context, LOCAL_NETWORK_PERMISSION) != PackageManager.PERMISSION_GRANTED
                    ) {
                        state.pendingPermissionDiscovery = true
                        localNetworkPermission.launch(LOCAL_NETWORK_PERMISSION)
                    } else {
                        discover()
                    }
                }
                Screen.BACKUP -> Backup(state, node, store, activeConnection, scope, page) { sourcePicker.launch(null) }
                Screen.RESTORE -> Restore(state, node, store, activeConnection, scope, page) { targetPicker.launch(null) }
                Screen.SETTINGS -> Settings(
                    state,
                    node,
                    store,
                    activeConnection,
                    embeddedProvider,
                    embeddedManager.activeMode(),
                    scope,
                    page,
                    createSettings::launch,
                    importSettings::launch,
                    onLanChange = { enabled ->
                        if (
                            enabled && Build.VERSION.SDK_INT >= 37 &&
                            ContextCompat.checkSelfPermission(context, LOCAL_NETWORK_PERMISSION) != PackageManager.PERMISSION_GRANTED
                        ) {
                            state.pendingPermissionLanEnable = true
                            localNetworkPermission.launch(LOCAL_NETWORK_PERMISSION)
                        } else {
                            setLan(enabled)
                        }
                    },
                    onSelectExternal = {
                        embeddedManager.selectExternalMode()
                        state.connectionHealth = ConnectionHealth.CONNECTING
                    },
                    onSelectLocal = {
                        if (embeddedManager.selectLocalMode()) {
                            state.connectionHealth = ConnectionHealth.CONNECTING
                        } else {
                            state.notice = resources.getString(R.string.phone_provider_mode_unavailable)
                        }
                    },
                    onTogglePhoneProvider = { enabled ->
                        if (enabled) requestProviderEnable() else embeddedManager.disable()
                    },
                )
            }
        }
    }
}

@Composable
private fun Home(
    state: CovalentViewModel,
    node: CovalentNodeClient,
    store: SecureNodeStore,
    connection: NodeConnection?,
    scope: kotlinx.coroutines.CoroutineScope,
    modifier: Modifier,
) {
    val context = LocalContext.current
    val resources = LocalResources.current
    LazyColumn(
        modifier,
        contentPadding = PaddingValues(20.dp, 14.dp, 20.dp, 104.dp),
        verticalArrangement = Arrangement.spacedBy(16.dp),
    ) {
        item {
            Column {
                Text(
                    stringResource(R.string.home_title),
                    style = MaterialTheme.typography.headlineMedium,
                    fontWeight = FontWeight.SemiBold,
                    modifier = Modifier.semantics { heading() },
                )
                Spacer(Modifier.height(8.dp))
                Text(
                    stringResource(R.string.home_subtitle),
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
        item {
            ConnectionCard(state) {
                connection?.let { reconnectNode(context, state, node, store, it, scope) }
            }
        }
        if (state.transfers.isNotEmpty()) {
            item { SectionTitle(stringResource(R.string.transfers_title)) }
            items(state.transfers, key = TransferRecord::jobId) { record ->
                TransferCard(record) { action ->
                    connection?.let { controlTransfer(context, state, node, store, it, scope, record, action) }
                }
            }
        }
        item {
            Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.fillMaxWidth()) {
                SectionTitle(stringResource(R.string.remembered_backups), Modifier.weight(1f))
                IconButton(
                    enabled = state.connectionHealth == ConnectionHealth.READY && !state.busy,
                    onClick = { connection?.let { reconnectNode(context, state, node, store, it, scope) } },
                ) {
                    Icon(Icons.Rounded.Refresh, stringResource(R.string.action_refresh_backups))
                }
            }
        }
        if (state.backups.isEmpty()) {
            item {
                EmptyState(
                    stringResource(R.string.no_backups_title),
                    stringResource(R.string.no_backups_detail),
                )
            }
        }
        items(state.backups, key = RememberedBackup::backupId) { backup ->
            BackupSummaryCard(
                backup = backup,
                providers = state.providers,
                verify = {
                    connection?.let { selectedConnection ->
                        api(context, state, scope) {
                            val snapshotId = backup.latestSnapshotId ?: return@api
                            val response = node.post(
                                selectedConnection.baseUrl,
                                selectedConnection.token,
                                "/api/v1/backups/verify",
                                JSONObject()
                                    .put("backupId", backup.backupId)
                                    .put("snapshotId", snapshotId)
                                    .put("verifyProviders", true),
                            )
                            state.notice = resources.getString(
                                if (response.getBoolean("intact")) R.string.verify_intact else R.string.verify_damaged,
                            )
                        }
                    }
                },
                manageReplicas = {
                    state.selectedBackupId = backup.backupId
                    state.backupName = backup.name
                    val eligible = state.providers.filter(::isProviderEligibleForBackup).map(Provider::peerId).toSet()
                    state.selectedProviderIds = backup.selectedProviderIds.intersect(eligible)
                    state.screen = Screen.BACKUP
                },
            )
        }
    }
}

@Composable
private fun ConnectionCard(state: CovalentViewModel, reconnect: () -> Unit) {
    val ready = state.connectionHealth == ConnectionHealth.READY
    val lastVerified = state.lastConnectedAtUnixMs
    val title = when (state.connectionHealth) {
        ConnectionHealth.READY -> stringResource(R.string.connection_ready)
        ConnectionHealth.CONNECTING -> stringResource(R.string.connection_checking)
        ConnectionHealth.STALE -> stringResource(R.string.connection_stale)
        ConnectionHealth.DISCONNECTED -> stringResource(R.string.connection_disconnected)
    }
    val detail = when {
        ready && state.status != null -> stringResource(
            R.string.connection_ready_detail,
            state.status!!.deviceName,
            state.status!!.protocolVersion.toInt(),
        )
        state.connectionError != null -> state.connectionError.orEmpty()
        else -> stringResource(R.string.connection_actions_disabled)
    }
    Card(Modifier.fillMaxWidth()) {
        Row(
            Modifier.padding(18.dp),
            horizontalArrangement = Arrangement.spacedBy(14.dp),
            verticalAlignment = Alignment.Top,
        ) {
            Surface(
                shape = MaterialTheme.shapes.medium,
                color = if (ready) MaterialTheme.colorScheme.secondaryContainer else MaterialTheme.colorScheme.errorContainer,
            ) {
                Icon(
                    if (ready) Icons.Rounded.Storage else Icons.Rounded.CloudOff,
                    contentDescription = null,
                    Modifier.padding(12.dp),
                )
            }
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(5.dp)) {
                Text(title, style = MaterialTheme.typography.titleLarge, fontWeight = FontWeight.SemiBold)
                Text(detail, color = MaterialTheme.colorScheme.onSurfaceVariant)
                if (!ready && lastVerified != null) {
                    Text(
                        stringResource(
                            R.string.connection_last_verified,
                            DateUtils.getRelativeTimeSpanString(lastVerified),
                        ),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                if (!ready) {
                    OutlinedButton(onClick = reconnect, enabled = !state.busy) {
                        Icon(Icons.Rounded.Refresh, contentDescription = null)
                        Text(stringResource(R.string.action_reconnect), Modifier.padding(start = 8.dp))
                    }
                }
            }
        }
    }
}

@Composable
private fun BackupSummaryCard(
    backup: RememberedBackup,
    providers: List<Provider>,
    verify: () -> Unit,
    manageReplicas: () -> Unit,
) {
    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(18.dp), verticalArrangement = Arrangement.spacedBy(5.dp)) {
            Text(backup.name, style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold)
            Text(
                if (backup.latestSnapshotId != null) {
                    pluralStringResource(
                        R.plurals.backup_snapshot_count,
                        backup.snapshotCount.toInt(),
                        backup.snapshotCount,
                    )
                } else {
                    stringResource(R.string.backup_no_snapshot)
                },
                style = MaterialTheme.typography.bodySmall,
            )
            if (backup.selectedProviderIds.isEmpty()) {
                Text(stringResource(R.string.backup_local_only), color = MaterialTheme.colorScheme.onSurfaceVariant)
            } else {
                Text(
                    pluralStringResource(
                        R.plurals.backup_replica_count,
                        backup.selectedProviderIds.size,
                        backup.selectedProviderIds.size,
                    ),
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                backup.selectedProviderIds.sorted().forEach { providerId ->
                    val provider = providers.firstOrNull { it.peerId == providerId }
                    val status = when (provider?.reachability) {
                        ProviderReachability.REACHABLE -> stringResource(R.string.provider_connected_state)
                        ProviderReachability.UNREACHABLE, null -> stringResource(R.string.provider_offline_state)
                        ProviderReachability.UNKNOWN -> stringResource(R.string.provider_unknown_state)
                    }
                    Text(
                        stringResource(
                            R.string.backup_replica_status,
                            provider?.displayName ?: provider?.address ?: stringResource(R.string.provider_unnamed),
                            status,
                        ),
                        style = MaterialTheme.typography.bodySmall,
                        color = if (provider?.let(::isProviderEligibleForBackup) == true) {
                            MaterialTheme.colorScheme.primary
                        } else {
                            MaterialTheme.colorScheme.error
                        },
                    )
                }
            }
            FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                if (backup.latestSnapshotId != null) {
                    OutlinedButton(onClick = verify) { Text(stringResource(R.string.action_verify)) }
                }
                OutlinedButton(onClick = manageReplicas) {
                    Text(stringResource(R.string.action_change_replicas))
                }
            }
            Text(
                stringResource(R.string.replica_change_existing_impact),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

private enum class TransferAction { PAUSE, RESUME, RETRY, CANCEL }

@Composable
private fun TransferCard(record: TransferRecord, onAction: (TransferAction) -> Unit) {
    val context = LocalContext.current
    var confirmCancel by remember(record.jobId) { mutableStateOf(false) }
    if (confirmCancel) {
        DestructiveConfirmDialog(
            action = DestructiveAction.CANCEL_TRANSFER,
            subject = record.label,
            onConfirm = {
                confirmCancel = false
                onAction(TransferAction.CANCEL)
            },
            onDismiss = { confirmCancel = false },
        )
    }
    val stateText = when (record.state) {
        TransferState.QUEUED -> stringResource(R.string.transfer_state_queued)
        TransferState.RUNNING -> stringResource(R.string.transfer_state_running)
        TransferState.PAUSED -> stringResource(R.string.transfer_state_paused)
        TransferState.COMPLETED -> stringResource(R.string.transfer_state_completed)
        TransferState.FAILED -> stringResource(R.string.transfer_state_failed)
        TransferState.CANCELLED -> stringResource(R.string.transfer_state_cancelled)
    }
    OutlinedCard(
        Modifier.fillMaxWidth().semantics(mergeDescendants = true) {
            contentDescription = "$stateText, ${record.label}"
        },
    ) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Icon(
                    if (record.state == TransferState.FAILED) Icons.Rounded.ErrorOutline else Icons.Rounded.Backup,
                    contentDescription = null,
                )
                Text(
                    record.label,
                    style = MaterialTheme.typography.titleMedium,
                    modifier = Modifier.padding(start = 10.dp).weight(1f),
                    maxLines = 2,
                    overflow = TextOverflow.Ellipsis,
                )
                Text(stateText, style = MaterialTheme.typography.labelLarge)
            }
            if (record.state == TransferState.RUNNING || record.state == TransferState.QUEUED) {
                val total = record.totalBytes
                if (total != null && total > 0) {
                    LinearProgressIndicator(
                        progress = { (record.completedBytes.toFloat() / total).coerceIn(0f, 1f) },
                        modifier = Modifier.fillMaxWidth(),
                    )
                } else {
                    LinearProgressIndicator(modifier = Modifier.fillMaxWidth())
                }
            }
            if (record.completedBytes > 0 || record.completedEntries > 0) {
                Text(
                    pluralStringResource(
                        R.plurals.transfer_progress_detail,
                        record.completedEntries.toInt(),
                        record.completedEntries,
                        Formatter.formatShortFileSize(context, record.completedBytes),
                    ),
                    style = MaterialTheme.typography.bodySmall,
                )
            }
            if (record.detail.isNotBlank()) {
                Text(record.detail, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
            FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                when (record.state) {
                    TransferState.RUNNING, TransferState.QUEUED -> {
                        TextButton(onClick = { onAction(TransferAction.PAUSE) }) {
                            Icon(Icons.Rounded.Pause, contentDescription = null)
                            Text(stringResource(R.string.action_pause))
                        }
                        TextButton(
                            onClick = { confirmCancel = true },
                            modifier = Modifier.testTag("transfer.cancel"),
                        ) {
                            Icon(Icons.Rounded.Cancel, contentDescription = null)
                            Text(stringResource(R.string.action_cancel))
                        }
                    }
                    TransferState.PAUSED -> {
                        TextButton(onClick = { onAction(TransferAction.RESUME) }) {
                            Icon(Icons.Rounded.PlayArrow, contentDescription = null)
                            Text(stringResource(R.string.action_resume))
                        }
                        TextButton(
                            onClick = { confirmCancel = true },
                            modifier = Modifier.testTag("transfer.cancel"),
                        ) {
                            Icon(Icons.Rounded.Cancel, contentDescription = null)
                            Text(stringResource(R.string.action_cancel))
                        }
                    }
                    TransferState.FAILED -> if (record.retryable) {
                        TextButton(onClick = { onAction(TransferAction.RETRY) }) {
                            Icon(Icons.Rounded.Refresh, contentDescription = null)
                            Text(stringResource(R.string.action_retry))
                        }
                    }
                    TransferState.COMPLETED, TransferState.CANCELLED -> Unit
                }
            }
        }
    }
}

private fun controlTransfer(
    context: Context,
    state: CovalentViewModel,
    node: CovalentNodeClient,
    store: SecureNodeStore,
    connection: NodeConnection,
    scope: kotlinx.coroutines.CoroutineScope,
    record: TransferRecord,
    action: TransferAction,
) {
    api(context, state, scope) {
        when (action) {
            TransferAction.PAUSE -> {
                runCatching { node.controlJob(connection.baseUrl, connection.token, record.jobId, "pause") }
                store.updateTransfer(record.jobId) {
                    it.copy(
                        state = TransferState.PAUSED,
                        detail = context.getString(R.string.transfer_paused_detail),
                        retryable = true,
                    )
                }
                TransferScheduler.cancelScheduled(context, record.jobId)
            }
            TransferAction.RESUME, TransferAction.RETRY -> {
                runCatching { node.controlJob(connection.baseUrl, connection.token, record.jobId, "resume") }
                check(store.pending(record.jobId) != null) {
                    context.getString(R.string.error_transfer_request_missing)
                }
                store.updateTransfer(record.jobId) {
                    it.copy(
                        state = TransferState.QUEUED,
                        detail = context.getString(R.string.transfer_queued_detail),
                        retryable = false,
                    )
                }
                TransferScheduler.enqueue(context, record.jobId)
            }
            TransferAction.CANCEL -> {
                runCatching {
                    node.controlJob(connection.baseUrl, connection.token, record.jobId, "cancel")
                }
                TransferScheduler.cancelScheduled(context, record.jobId)
                val discarded = runCatching {
                    node.discardJob(connection.baseUrl, connection.token, record.jobId)
                }
                val confirmedDetail = context.getString(R.string.transfer_cancelled_detail)
                if (discarded.isSuccess) {
                    store.removePendingDiscard(record.jobId)
                } else {
                    store.savePendingDiscard(record.jobId, confirmedDetail)
                }
                store.updateTransfer(record.jobId) {
                    it.copy(
                        state = TransferState.CANCELLED,
                        detail = if (discarded.isSuccess) confirmedDetail else {
                            context.getString(R.string.transfer_cancelled_unconfirmed_detail)
                        },
                        retryable = false,
                    )
                }
                store.removePending(record.jobId)
                store.removePendingAcknowledgement(record.jobId)
            }
        }
        state.refreshDurableState()
    }
}

@Composable
private fun Setup(
    state: CovalentViewModel,
    modifier: Modifier,
    connect: () -> Unit,
    pickTokenFile: () -> Unit,
    pickCaCertificate: () -> Unit,
) {
    val nameFocus = remember { FocusRequester() }
    val addressFocus = remember { FocusRequester() }
    val tokenFocus = remember { FocusRequester() }
    val canSubmit = !state.busy
    FormPage(
        modifier,
        stringResource(R.string.setup_title),
        stringResource(R.string.setup_subtitle),
    ) {
        OnboardingChoice(
            icon = Icons.Rounded.Search,
            title = stringResource(R.string.setup_nearby_title),
            detail = stringResource(R.string.setup_nearby_detail),
        )
        OnboardingChoice(
            icon = Icons.Rounded.Security,
            title = stringResource(R.string.setup_handoff_title),
            detail = stringResource(R.string.setup_handoff_detail),
        )
        OutlinedTextField(
            state.setupName,
            {
                state.setupName = it
                state.setupNameError = ""
            },
            label = { Text(stringResource(R.string.field_device_name)) },
            isError = state.setupNameError.isNotEmpty(),
            supportingText = state.setupNameError.takeIf(String::isNotEmpty)?.let { error -> ({ Text(error) }) },
            singleLine = true,
            modifier = Modifier.fillMaxWidth().focusRequester(nameFocus),
        )
        OutlinedTextField(
            state.setupAddress,
            {
                state.setupAddress = it
                state.setupAddressError = ""
            },
            label = { Text(stringResource(R.string.field_node_address)) },
            placeholder = { Text(stringResource(R.string.node_address_example)) },
            isError = state.setupAddressError.isNotEmpty(),
            supportingText = state.setupAddressError.takeIf(String::isNotEmpty)?.let { error -> ({ Text(error) }) },
            singleLine = true,
            keyboardOptions = KeyboardOptions(
                capitalization = KeyboardCapitalization.None,
                autoCorrectEnabled = false,
                keyboardType = KeyboardType.Uri,
                imeAction = ImeAction.Next,
            ),
            modifier = Modifier.fillMaxWidth().focusRequester(addressFocus),
        )
        OutlinedTextField(
            state.setupToken,
            {
                state.setupToken = it
                state.setupTokenError = ""
            },
            label = { Text(stringResource(R.string.field_node_token)) },
            visualTransformation = PasswordVisualTransformation(),
            isError = state.setupTokenError.isNotEmpty(),
            supportingText = state.setupTokenError.takeIf(String::isNotEmpty)?.let { error -> ({ Text(error) }) },
            singleLine = true,
            keyboardOptions = KeyboardOptions(
                capitalization = KeyboardCapitalization.None,
                autoCorrectEnabled = false,
                keyboardType = KeyboardType.Password,
                imeAction = ImeAction.Done,
            ),
            keyboardActions = KeyboardActions(onDone = { if (canSubmit) connect() }),
            modifier = Modifier.fillMaxWidth().focusRequester(tokenFocus),
        )
        Row(
            modifier = Modifier.fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text(
                stringResource(R.string.token_file_detail),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.weight(1f),
            )
            OutlinedButton(onClick = pickTokenFile) {
                Text(stringResource(R.string.action_choose_token_file))
            }
        }
        Text(
            stringResource(R.string.setup_transport_policy),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        SectionTitle(stringResource(R.string.tls_enrollment_title))
        Text(
            stringResource(R.string.tls_enrollment_detail),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        OutlinedButton(onClick = pickCaCertificate) {
            Icon(Icons.Rounded.Security, contentDescription = null)
            Text(stringResource(R.string.action_choose_ca_certificate), Modifier.padding(start = 8.dp))
        }
        if (state.setupCaCertificateDer.isNotBlank()) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    if (state.setupCaCertificateLabel == "saved") {
                        stringResource(R.string.ca_certificate_enrolled)
                    } else {
                        state.setupCaCertificateLabel
                    },
                    modifier = Modifier.weight(1f),
                )
                TextButton(onClick = {
                    state.setupCaCertificateDer = ""
                    state.setupCaCertificateLabel = ""
                }) { Text(stringResource(R.string.action_clear)) }
            }
        }
        Text(
            stringResource(R.string.tls_enrollment_or_pin),
            style = MaterialTheme.typography.labelLarge,
        )
        OutlinedTextField(
            state.setupCertificatePin,
            {
                state.setupCertificatePin = it
                state.setupCertificatePinError = ""
                if (it.isNotBlank()) {
                    state.setupCaCertificateDer = ""
                    state.setupCaCertificateLabel = ""
                }
            },
            label = { Text(stringResource(R.string.field_certificate_pin)) },
            placeholder = { Text(stringResource(R.string.certificate_pin_example)) },
            supportingText = if (state.setupCertificatePinError.isNotBlank()) {
                { Text(state.setupCertificatePinError) }
            } else {
                { Text(stringResource(R.string.certificate_pin_detail)) }
            },
            isError = state.setupCertificatePinError.isNotBlank(),
            keyboardOptions = pairingInvitationKeyboardOptions,
            modifier = Modifier.fillMaxWidth(),
        )
        Text(
            stringResource(R.string.caddy_ca_guidance),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        if (requiresLocalNetworkPermission(state.setupAddress, Build.VERSION.SDK_INT)) {
            Text(
                stringResource(R.string.setup_lan_permission_rationale),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.primary,
            )
        }
        if (state.setupConnectionError.isNotBlank()) {
            InlineError(state.setupConnectionError)
        }
        FilledTonalButton(enabled = canSubmit, onClick = {
            when {
                state.setupName.isBlank() -> nameFocus.requestFocus()
                validateNodeAddress(state.setupAddress) != AddressIssue.NONE -> addressFocus.requestFocus()
                state.setupToken.isBlank() -> tokenFocus.requestFocus()
            }
            connect()
        }) {
            if (state.busy) CircularProgressIndicator(Modifier.padding(end = 8.dp))
            Text(stringResource(if (state.busy) R.string.action_checking else R.string.action_connect))
        }
    }
}

@Composable
private fun OnboardingChoice(icon: ImageVector, title: String, detail: String) {
    // These are setup instructions, not actions. A plain layout avoids the
    // tappable-card affordance until the app has a connected server to act on.
    Row(
        Modifier.fillMaxWidth().padding(vertical = 6.dp),
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Icon(icon, contentDescription = null)
        Column {
            Text(title, fontWeight = FontWeight.SemiBold)
            Text(detail, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
    }
}

@Composable
private fun Pair(
    state: CovalentViewModel,
    node: CovalentNodeClient,
    connection: NodeConnection?,
    scope: kotlinx.coroutines.CoroutineScope,
    modifier: Modifier,
    discover: () -> Unit,
) {
    val context = LocalContext.current
    val resources = LocalResources.current
    val focusManager = LocalFocusManager.current
    var confirmDiscardPairing by remember { mutableStateOf(false) }
    if (confirmDiscardPairing) {
        DestructiveConfirmDialog(
            action = DestructiveAction.DISCARD_PAIRING_PROGRESS,
            onConfirm = {
                confirmDiscardPairing = false
                state.clearPairing()
                state.notice = resources.getString(R.string.pair_local_state_cleared)
            },
            onDismiss = { confirmDiscardPairing = false },
        )
    }
    fun startNetworkPairing(address: String) {
        val selectedConnection = connection ?: return
        val candidateAddress = address.trim()
        if (candidateAddress.isEmpty()) return
        api(context, state, scope, pairingError = true) {
            val pairing = node.startNetworkPairing(selectedConnection.baseUrl, selectedConnection.token, candidateAddress)
            state.updateNetworkPairing(pairing)
            if (pairing.state == NetworkPairingState.COMPLETE) {
                completeNetworkPairingConnection(state, node, selectedConnection, pairing)
            }
        }
    }
    FormPage(
        modifier,
        stringResource(R.string.pair_title),
        stringResource(R.string.pair_subtitle),
    ) {
        DiscoverySection(state, discover) { candidate -> startNetworkPairing(candidate.endpoint) }
        SectionTitle(stringResource(R.string.tailscale_candidate_title))
        OutlinedTextField(
            value = state.tailscaleCandidateAddress,
            onValueChange = { state.tailscaleCandidateAddress = it; state.pairingError = "" },
            label = { Text(stringResource(R.string.tailscale_candidate_field)) },
            placeholder = { Text(stringResource(R.string.tailscale_candidate_example)) },
            supportingText = {
                Text(
                    stringResource(
                        if (state.status?.lanDiscovery == false) {
                            R.string.tailscale_candidate_lan_off_detail
                        } else {
                            R.string.tailscale_candidate_detail
                        },
                    ),
                )
            },
            singleLine = true,
            keyboardOptions = KeyboardOptions(
                capitalization = KeyboardCapitalization.None,
                autoCorrectEnabled = false,
                keyboardType = KeyboardType.Uri,
                imeAction = ImeAction.Done,
            ),
            keyboardActions = KeyboardActions(onDone = {
                focusManager.clearFocus()
                startNetworkPairing(state.tailscaleCandidateAddress)
            }),
            modifier = Modifier.fillMaxWidth().testTag("pair.tailscaleAddress"),
        )
        FilledTonalButton(
            enabled = state.tailscaleCandidateAddress.trim().isNotEmpty() && !state.busy &&
                state.connectionHealth == ConnectionHealth.READY,
            onClick = { startNetworkPairing(state.tailscaleCandidateAddress) },
            modifier = Modifier.testTag("pair.tailscaleStart"),
        ) {
            Icon(Icons.Rounded.AddLink, contentDescription = null)
            Text(stringResource(R.string.action_use_backup_device), Modifier.padding(start = 8.dp))
        }
        state.activeNetworkPairing?.let { pairing ->
            NetworkPairingCard(
                pairing = pairing,
                providerPersisted = pairing.peerTransport?.let { transport ->
                    state.providers.any {
                        it.peerId == transport.peerId && it.fingerprint == transport.certificateFingerprint
                    }
                } == true,
                busy = state.busy,
                confirm = {
                    connection?.let { selectedConnection ->
                        api(context, state, scope, pairingError = true) {
                            val updated = node.confirmNetworkPairing(
                                selectedConnection.baseUrl,
                                selectedConnection.token,
                                pairing.pairingId,
                                pairing.authenticationString,
                            )
                            state.updateNetworkPairing(updated)
                            if (updated.state == NetworkPairingState.COMPLETE) {
                                completeNetworkPairingConnection(state, node, selectedConnection, updated)
                            }
                        }
                    }
                },
                dismiss = {
                    connection?.let { selectedConnection ->
                        api(context, state, scope, pairingError = true) {
                            node.cancelNetworkPairing(selectedConnection.baseUrl, selectedConnection.token, pairing.pairingId)
                            state.removeNetworkPairing(pairing.pairingId)
                        }
                    }
                },
            )
        }
        if (state.pairingError.isNotBlank()) InlineError(state.pairingError)
        TextButton(
            onClick = { state.showAdvancedPairing = !state.showAdvancedPairing },
            modifier = Modifier.testTag("pair.advanced"),
        ) {
            Icon(Icons.Rounded.Security, contentDescription = null)
            Text(
                stringResource(
                    if (state.showAdvancedPairing) R.string.action_hide_advanced_pairing
                    else R.string.action_show_advanced_pairing,
                ),
                Modifier.padding(start = 8.dp),
            )
        }
        if (state.showAdvancedPairing && connection != null) {
            HorizontalDivider()
            Text(
                stringResource(R.string.advanced_pairing_detail),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                FilterChip(
                    selected = state.pairingRole == PairingRole.INVITER,
                    onClick = {
                        if (state.pairingRole != PairingRole.INVITER) state.clearPairing()
                        state.pairingRole = PairingRole.INVITER
                    },
                    label = { Text(stringResource(R.string.pair_invite_device)) },
                )
                FilterChip(
                    selected = state.pairingRole == PairingRole.RESPONDER,
                    onClick = {
                        if (state.pairingRole != PairingRole.RESPONDER) state.clearPairing()
                        state.pairingRole = PairingRole.RESPONDER
                    },
                    label = { Text(stringResource(R.string.pair_join_device)) },
                )
            }
            if (state.pairingRole == PairingRole.INVITER) {
                InviterPairing(state, node, connection, scope)
            } else {
                ResponderPairing(state, node, connection, scope, focusManager)
            }
            if (state.pairingConfirmation != null) {
                ProviderExchange(state, node, connection, scope)
            }
            TextButton(
                onClick = { confirmDiscardPairing = true },
                modifier = Modifier.testTag("pair.discardAdvanced"),
            ) {
                Icon(Icons.Rounded.Cancel, contentDescription = null)
                Text(stringResource(R.string.action_cancel_pairing))
            }
        }
    }
}

@Composable
private fun DiscoverySection(
    state: CovalentViewModel,
    discover: () -> Unit,
    select: (DiscoveryCandidate) -> Unit,
) {
    Row(horizontalArrangement = Arrangement.spacedBy(10.dp), verticalAlignment = Alignment.CenterVertically) {
        OutlinedButton(
            enabled = !state.busy && state.connectionHealth == ConnectionHealth.READY,
            onClick = discover,
        ) {
            Icon(Icons.Rounded.Search, contentDescription = null)
            Text(stringResource(R.string.action_find_devices), Modifier.padding(start = 8.dp))
        }
        if (state.discoveryRunning) {
            CircularProgressIndicator()
            Text(stringResource(R.string.discovery_searching))
        }
    }
    state.discoveryError?.let { InlineError(it) }
    if (state.discoveryCompleted && state.discovered.isEmpty() && state.discoveryError == null) {
        EmptyState(
            stringResource(R.string.discovery_empty_title),
            stringResource(
                if (state.status?.lanDiscovery == false) R.string.discovery_lan_off_detail
                else R.string.discovery_empty_detail,
            ),
        )
    }
    state.discovered.forEach { candidate ->
        DiscoveryCandidateRow(candidate) { select(candidate) }
    }
}

@Composable
private fun DiscoveryCandidateRow(candidate: DiscoveryCandidate, select: () -> Unit) {
    AssistChip(
        onClick = select,
        label = {
            Text(
                stringResource(R.string.discovery_candidate, candidate.source, candidate.endpoint),
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
            )
        },
    )
}

@Composable
private fun NetworkPairingCard(
    pairing: NetworkPairing,
    providerPersisted: Boolean,
    busy: Boolean,
    confirm: () -> Unit,
    dismiss: () -> Unit,
) {
    val direction = stringResource(
        if (pairing.direction == NetworkPairingDirection.INCOMING) {
            R.string.network_pairing_incoming
        } else {
            R.string.network_pairing_outgoing
        },
    )
    var confirmCancelRequest by remember(pairing.pairingId) { mutableStateOf(false) }
    if (confirmCancelRequest) {
        DestructiveConfirmDialog(
            action = DestructiveAction.CANCEL_DEVICE_REQUEST,
            subject = pairing.peerName,
            onConfirm = {
                confirmCancelRequest = false
                dismiss()
            },
            onDismiss = { confirmCancelRequest = false },
        )
    }
    OutlinedCard(Modifier.fillMaxWidth().testTag("pair.networkRequest")) {
        Column(
            Modifier.padding(18.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Icon(Icons.Rounded.Security, contentDescription = null)
                Column(Modifier.padding(start = 10.dp).weight(1f)) {
                    Text(pairing.peerName, style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold)
                    Text(
                        direction,
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
            Text(
                stringResource(R.string.network_pairing_compare),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Text(
                pairing.authenticationString,
                style = MaterialTheme.typography.headlineSmall,
                fontFamily = FontFamily.Monospace,
                modifier = Modifier
                    .fillMaxWidth()
                    .testTag("pair.authenticationString")
                    .clearAndSetSemantics {
                        contentDescription = pairing.authenticationString.replace('-', ' ')
                    },
            )
            Text(
                stringResource(
                    R.string.network_pairing_expires,
                    DateUtils.getRelativeTimeSpanString(pairing.expiresAtUnixMs),
                ),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            when (pairing.state) {
                NetworkPairingState.AWAITING_LOCAL_CONFIRMATION -> {
                    FilledTonalButton(
                        enabled = !busy,
                        onClick = confirm,
                        modifier = Modifier.testTag("pair.confirmNetwork"),
                    ) {
                        Icon(Icons.Rounded.CheckCircle, contentDescription = null)
                        Text(stringResource(R.string.action_confirm_backup_device), Modifier.padding(start = 8.dp))
                    }
                }
                NetworkPairingState.AWAITING_PEER_CONFIRMATION -> {
                    Row(horizontalArrangement = Arrangement.spacedBy(10.dp), verticalAlignment = Alignment.CenterVertically) {
                        CircularProgressIndicator()
                        Text(stringResource(R.string.network_pairing_waiting_peer))
                    }
                }
                NetworkPairingState.COMPLETE -> {
                    if (providerPersisted) {
                        Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
                            Icon(Icons.Rounded.CheckCircle, contentDescription = null, tint = MaterialTheme.colorScheme.primary)
                            Text(stringResource(R.string.network_pairing_complete), fontWeight = FontWeight.SemiBold)
                        }
                    } else {
                        Row(horizontalArrangement = Arrangement.spacedBy(10.dp), verticalAlignment = Alignment.CenterVertically) {
                            CircularProgressIndicator()
                            Text(stringResource(R.string.network_pairing_securing_provider))
                        }
                    }
                }
                NetworkPairingState.FAILED -> InlineError(
                    pairing.failureMessage ?: stringResource(R.string.network_pairing_failed),
                )
            }
            if (pairing.state == NetworkPairingState.FAILED ||
                pairing.state == NetworkPairingState.COMPLETE && providerPersisted
            ) {
                FilledTonalButton(enabled = !busy, onClick = dismiss) {
                    Text(stringResource(R.string.action_done))
                }
            } else {
                TextButton(
                    enabled = !busy,
                    onClick = { confirmCancelRequest = true },
                    modifier = Modifier.testTag("pair.cancelNetworkRequest"),
                ) {
                    Text(stringResource(R.string.action_cancel_pairing))
                }
            }
        }
    }
}

internal fun validateSignedProviderBinding(transport: PeerTransport, connected: Provider) {
    check(transport.certificateFingerprint.matches(Regex("[0-9a-f]{64}"))) {
        "The signed certificate fingerprint is invalid."
    }
    check(connected.peerId == transport.peerId &&
        connected.fingerprint == transport.certificateFingerprint
    ) {
        "The connected provider certificate does not match the signed pairing."
    }
}

internal fun completeNetworkPairingConnection(
    state: CovalentViewModel,
    node: CovalentNodeClient,
    connection: NodeConnection,
    pairing: NetworkPairing,
) {
    check(pairing.state == NetworkPairingState.COMPLETE) {
        "The pairing is not complete."
    }
    val transport = checkNotNull(pairing.peerTransport) {
        "The completed pairing omitted its signed transport."
    }
    val current = node.providers(connection.baseUrl, connection.token)
    val connected = current.firstOrNull { it.peerId == transport.peerId }
        ?: node.connectProvider(
            connection.baseUrl,
            connection.token,
            transport,
        )
    validateSignedProviderBinding(transport, connected)
    val persisted = node.providers(connection.baseUrl, connection.token)
    check(persisted.any {
        it.peerId == transport.peerId && it.fingerprint == transport.certificateFingerprint
    }) {
        "The signed provider connection was not persisted."
    }
    state.providers = persisted
}

@Composable
private fun InviterPairing(
    state: CovalentViewModel,
    node: CovalentNodeClient,
    connection: NodeConnection,
    scope: kotlinx.coroutines.CoroutineScope,
) {
    val context = LocalContext.current
    val resources = LocalResources.current
    if (state.pairingInvitation == null) {
        FilledTonalButton(
            enabled = !state.busy,
            onClick = {
                api(context, state, scope, pairingError = true) {
                    val invitation = node.post(
                        connection.baseUrl,
                        connection.token,
                        "/api/v1/pair/invitations",
                        JSONObject()
                            .put("lifetimeMs", 15 * 60 * 1_000)
                            .put("endpoints", JSONArray()),
                    )
                    state.persistPairingInvitation(invitation)
                }
            },
        ) { Text(stringResource(R.string.action_create_invitation)) }
    }
    state.pairingInvitation?.let { invitation ->
        ExchangeCard(
            title = stringResource(R.string.pair_invitation_ready),
            detail = remainingMinutes(invitation).let { minutes ->
                pluralStringResource(R.plurals.pair_invitation_expiry, minutes.toInt(), minutes)
            },
            json = invitation,
            shareLabel = stringResource(R.string.action_share_invitation),
        )
        PairingInput(
            value = state.pairingInput,
            onValueChange = { state.pairingInput = it; state.pairingError = "" },
            label = stringResource(R.string.field_responder_session),
            actionLabel = stringResource(R.string.action_load_session),
            enabled = !state.busy,
        ) {
            runCatching {
                val session = parseBoundedJson(state.pairingInput)
                requireSameInvitation(invitation, session)
                state.persistPairingSession(session)
                state.pairingError = ""
            }.onFailure {
                Log.w(UI_LOG_TAG, "Pairing exchange could not be read", it)
                state.pairingError = resources.getString(R.string.error_invalid_pairing_exchange)
            }
        }
    }
    state.pairingSession?.let { session ->
        PairingConsent(session)
        val inviterConfirmed = !session.isNull("inviterConfirmationSignature")
        val responderConfirmed = !session.isNull("responderConfirmationSignature")
        when {
            !responderConfirmed -> InlineError(stringResource(R.string.pair_wait_responder_confirmation))
            !inviterConfirmed -> FilledTonalButton(enabled = !state.busy && !isExpired(session), onClick = {
                api(context, state, scope, pairingError = true) {
                    val confirmed = node.post(
                        connection.baseUrl,
                        connection.token,
                        "/api/v1/pair/confirm/inviter",
                        JSONObject()
                            .put("session", session)
                            .put("displayedCode", session.getString("authenticationString")),
                    )
                    state.persistPairingSession(confirmed)
                }
            }) {
                Icon(Icons.Rounded.Security, contentDescription = null)
                Text(stringResource(R.string.action_confirm_inviter), Modifier.padding(start = 8.dp))
            }
            state.pairingConfirmation == null -> {
                ExchangeCard(
                    title = stringResource(R.string.pair_mutual_session_ready),
                    detail = stringResource(R.string.pair_return_session_detail),
                    json = session,
                    shareLabel = stringResource(R.string.action_share_signed_session),
                )
                FilledTonalButton(enabled = !state.busy && !isExpired(session), onClick = {
                    api(context, state, scope, pairingError = true) {
                        state.persistPairingConfirmation(
                            node.post(
                                connection.baseUrl,
                                connection.token,
                                "/api/v1/pair/finalize/inviter",
                                JSONObject().put("session", session),
                            ),
                        )
                    }
                }) { Text(stringResource(R.string.action_finalize_inviter)) }
            }
        }
    }
}

@Composable
private fun ResponderPairing(
    state: CovalentViewModel,
    node: CovalentNodeClient,
    connection: NodeConnection,
    scope: kotlinx.coroutines.CoroutineScope,
    focusManager: androidx.compose.ui.focus.FocusManager,
) {
    val context = LocalContext.current
    val resources = LocalResources.current
    if (state.pairingSession == null) {
        OutlinedTextField(
            state.pairingDisplayName,
            { state.pairingDisplayName = it },
            label = { Text(stringResource(R.string.field_pairing_display_name)) },
            singleLine = true,
            modifier = Modifier.fillMaxWidth(),
        )
        RoleSelector(
            title = stringResource(R.string.roles_for_responder),
            detail = stringResource(R.string.roles_for_responder_detail),
            selected = roleSet(state.responderRolesText),
        ) { state.responderRolesText = it.sorted().joinToString(",") }
        RoleSelector(
            title = stringResource(R.string.roles_for_inviter),
            detail = stringResource(R.string.roles_for_inviter_detail),
            selected = roleSet(state.inviterRolesText),
        ) { state.inviterRolesText = it.sorted().joinToString(",") }
        PairingInput(
            value = state.pairingInput,
            onValueChange = { state.pairingInput = it; state.pairingError = "" },
            label = stringResource(R.string.field_pairing_invitation),
            actionLabel = stringResource(R.string.action_accept_invitation),
            enabled = state.pairingDisplayName.isNotBlank() && !state.busy,
        ) {
            focusManager.clearFocus()
            api(context, state, scope, pairingError = true) {
                val invitation = parseBoundedJson(state.pairingInput)
                check(!isExpiredInvitation(invitation)) { resources.getString(R.string.error_invitation_expired) }
                state.persistPairingInvitation(invitation)
                state.persistPairingSession(
                    node.post(
                        connection.baseUrl,
                        connection.token,
                        "/api/v1/pair/accept",
                        JSONObject()
                            .put("invitation", invitation)
                            .put("responderName", state.pairingDisplayName.trim())
                            .put("responderRoles", JSONArray(roleSet(state.responderRolesText).sorted()))
                            .put("inviterRoles", JSONArray(roleSet(state.inviterRolesText).sorted())),
                    ),
                )
            }
        }
    }
    state.pairingSession?.let { session ->
        PairingConsent(session)
        val responderConfirmed = !session.isNull("responderConfirmationSignature")
        val inviterConfirmed = !session.isNull("inviterConfirmationSignature")
        when {
            !responderConfirmed -> FilledTonalButton(enabled = !state.busy && !isExpired(session), onClick = {
                api(context, state, scope, pairingError = true) {
                    val confirmed = node.post(
                        connection.baseUrl,
                        connection.token,
                        "/api/v1/pair/confirm/responder",
                        JSONObject()
                            .put("session", session)
                            .put("displayedCode", session.getString("authenticationString")),
                    )
                    state.persistPairingSession(confirmed)
                }
            }) {
                Icon(Icons.Rounded.Security, contentDescription = null)
                Text(stringResource(R.string.action_confirm_responder), Modifier.padding(start = 8.dp))
            }
            !inviterConfirmed -> {
                ExchangeCard(
                    title = stringResource(R.string.pair_responder_session_ready),
                    detail = stringResource(R.string.pair_send_to_inviter_detail),
                    json = session,
                    shareLabel = stringResource(R.string.action_share_signed_session),
                )
                PairingInput(
                    value = state.pairingInput,
                    onValueChange = { state.pairingInput = it; state.pairingError = "" },
                    label = stringResource(R.string.field_mutual_session),
                    actionLabel = stringResource(R.string.action_load_updated_session),
                    enabled = !state.busy,
                ) {
                    runCatching {
                        val updated = parseBoundedJson(state.pairingInput)
                        requireSameSession(session, updated)
                        check(!updated.isNull("inviterConfirmationSignature")) {
                            resources.getString(R.string.error_inviter_not_confirmed)
                        }
                        state.persistPairingSession(updated)
                        state.pairingError = ""
                    }.onFailure {
                        Log.w(UI_LOG_TAG, "Pairing exchange could not be confirmed", it)
                        state.pairingError = resources.getString(R.string.error_invalid_pairing_exchange)
                    }
                }
            }
            state.pairingConfirmation == null -> FilledTonalButton(
                enabled = !state.busy && !isExpired(session),
                onClick = {
                    api(context, state, scope, pairingError = true) {
                        state.persistPairingConfirmation(
                            node.post(
                                connection.baseUrl,
                                connection.token,
                                "/api/v1/pair/finalize/responder",
                                JSONObject().put("session", session),
                            ),
                        )
                    }
                },
            ) { Text(stringResource(R.string.action_finalize_responder)) }
        }
    }
}

@Composable
private fun RoleSelector(title: String, detail: String, selected: Set<String>, onChange: (Set<String>) -> Unit) {
    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Text(title, style = MaterialTheme.typography.titleMedium)
        Text(detail, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        ALL_PAIRING_ROLES.forEach { roleName ->
            val checked = roleName in selected
            Row(
                Modifier
                    .fillMaxWidth()
                    .toggleable(
                        value = checked,
                        role = Role.Checkbox,
                        onValueChange = { enabled ->
                            onChange(if (enabled) selected + roleName else selected - roleName)
                        },
                    )
                    .semantics(mergeDescendants = true) {}
                    .padding(vertical = 8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Checkbox(checked = checked, onCheckedChange = null)
                Column(Modifier.padding(start = 10.dp)) {
                    Text(roleTitle(roleName))
                    Text(roleName, fontFamily = FontFamily.Monospace, style = MaterialTheme.typography.bodySmall)
                }
            }
        }
    }
}

@Composable
private fun roleTitle(role: String): String = when (role) {
    "storage_provider" -> stringResource(R.string.role_storage_provider)
    "backup_reader" -> stringResource(R.string.role_backup_reader)
    "backup_writer" -> stringResource(R.string.role_backup_writer)
    else -> role
}

@Composable
private fun PairingInput(
    value: String,
    onValueChange: (String) -> Unit,
    label: String,
    actionLabel: String,
    enabled: Boolean,
    onAction: () -> Unit,
) {
    val focusManager = LocalFocusManager.current
    OutlinedTextField(
        value,
        onValueChange,
        label = { Text(label) },
        minLines = 3,
        maxLines = 6,
        keyboardOptions = pairingInvitationKeyboardOptions,
        keyboardActions = KeyboardActions(onDone = { focusManager.clearFocus() }),
        modifier = Modifier.fillMaxWidth(),
    )
    FilledTonalButton(enabled = enabled && value.isNotBlank(), onClick = onAction) { Text(actionLabel) }
}

@Composable
private fun PairingConsent(session: JSONObject) {
    val invitation = session.getJSONObject("invitation")
    val none = stringResource(R.string.value_none)
    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(18.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(stringResource(R.string.pair_compare_code), style = MaterialTheme.typography.titleMedium)
            Text(
                session.getString("authenticationString"),
                style = MaterialTheme.typography.headlineSmall,
                fontWeight = FontWeight.Bold,
                fontFamily = FontFamily.Monospace,
            )
            Text(stringResource(R.string.pair_inviter_identity), fontWeight = FontWeight.SemiBold)
            Text(invitation.getString("inviterDeviceName"))
            Text(invitation.getString("inviterDeviceId"), fontFamily = FontFamily.Monospace)
            Text(
                stringResource(
                    R.string.pair_exact_roles,
                    jsonStrings(session.getJSONArray("inviterRoles")).joinToString(", ").ifBlank { none },
                ),
                style = MaterialTheme.typography.bodySmall,
            )
            Text(stringResource(R.string.pair_responder_identity), fontWeight = FontWeight.SemiBold)
            Text(session.getString("responderName"))
            Text(session.getString("responderDeviceId"), fontFamily = FontFamily.Monospace)
            Text(
                stringResource(
                    R.string.pair_exact_roles,
                    jsonStrings(session.getJSONArray("responderRoles")).joinToString(", ").ifBlank { none },
                ),
                style = MaterialTheme.typography.bodySmall,
            )
            Text(
                stringResource(R.string.pair_consent_warning),
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            if (isExpired(session)) InlineError(stringResource(R.string.error_invitation_expired))
        }
    }
}

@Composable
private fun ExchangeCard(title: String, detail: String, json: JSONObject, shareLabel: String) {
    val context = LocalContext.current
    val serialized = remember(json.toString()) { json.toString() }
    OutlinedCard(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(title, style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold)
            Text(detail, color = MaterialTheme.colorScheme.onSurfaceVariant)
            Text(
                serialized,
                maxLines = 4,
                overflow = TextOverflow.Ellipsis,
                fontFamily = FontFamily.Monospace,
                style = MaterialTheme.typography.bodySmall,
            )
            FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                OutlinedButton(onClick = { copyText(context, title, serialized) }) {
                    Icon(Icons.Rounded.ContentCopy, contentDescription = null)
                    Text(stringResource(R.string.action_copy), Modifier.padding(start = 8.dp))
                }
                OutlinedButton(onClick = { shareText(context, title, serialized) }) {
                    Icon(Icons.Rounded.AddLink, contentDescription = null)
                    Text(shareLabel, Modifier.padding(start = 8.dp))
                }
            }
        }
    }
}

@Composable
private fun ProviderExchange(
    state: CovalentViewModel,
    node: CovalentNodeClient,
    connection: NodeConnection,
    scope: kotlinx.coroutines.CoroutineScope,
) {
    val context = LocalContext.current
    val resources = LocalResources.current
    val confirmation = state.pairingConfirmation ?: return
    val peerGrant = confirmation.getJSONObject(
        if (state.pairingRole == PairingRole.INVITER) "inviterGrant" else "responderGrant",
    )
    val roles = jsonStrings(peerGrant.getJSONArray("roles"))
    val none = stringResource(R.string.value_none)
    StatusCard(
        stringResource(R.string.pairing_complete),
        peerGrant.getString("displayName"),
        stringResource(R.string.pair_exact_roles, roles.joinToString(", ").ifBlank { none }),
        Icons.Rounded.CheckCircle,
    )
    if ("storage_provider" !in roles) {
        Text(
            stringResource(R.string.provider_role_not_granted),
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    } else {
        val transport = runCatching { signedPeerTransport(confirmation) }.getOrNull()
        if (transport == null) {
            InlineError(stringResource(R.string.error_signed_transport_missing))
        } else {
            Text(
                stringResource(R.string.signed_transport_ready),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            FilledTonalButton(
                enabled = !state.busy,
                onClick = {
                    api(context, state, scope, pairingError = true) {
                        val connected = node.connectProvider(connection.baseUrl, connection.token, transport)
                        validateSignedProviderBinding(transport, connected)
                        val persisted = node.providers(connection.baseUrl, connection.token)
                        check(persisted.any {
                            it.peerId == transport.peerId &&
                                it.fingerprint == transport.certificateFingerprint
                        }) { resources.getString(R.string.error_provider_fingerprint_mismatch) }
                        state.providers = persisted
                        state.notice = resources.getString(R.string.provider_connected, transport.displayName)
                    }
                },
            ) { Text(stringResource(R.string.action_use_backup_device)) }
        }
    }
}

internal fun signedPeerTransport(confirmation: JSONObject): PeerTransport {
    val value = confirmation.getJSONObject("peerTransport")
    return PeerTransport(
        peerId = value.getString("peerId").also {
            require(it.matches(Regex("[A-Za-z0-9_-]{1,128}")))
        },
        displayName = value.getString("displayName").also {
            require(it.isNotBlank() && it.length <= 128)
        },
        address = value.getString("address").also {
            require(it.isNotBlank() && it.length <= 253 && it.none(Char::isWhitespace))
        },
        certificateDer = value.getString("certificateDer").also {
            require(it.isNotBlank() && it.length <= 512 * 1_024)
        },
        certificateFingerprint = value.getString("certificateFingerprint").also {
            require(it.matches(Regex("[0-9a-f]{64}")))
        },
    )
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun Backup(
    state: CovalentViewModel,
    node: CovalentNodeClient,
    store: SecureNodeStore,
    connection: NodeConnection?,
    scope: kotlinx.coroutines.CoroutineScope,
    modifier: Modifier,
    pickSource: () -> Unit,
) {
    val context = LocalContext.current
    val resources = LocalResources.current
    LaunchedEffect(connection?.baseUrl, connection?.token) {
        if (connection != null && state.providers.isEmpty() && state.connectionHealth == ConnectionHealth.READY) {
            runCatching { withContext(Dispatchers.IO) { node.providers(connection.baseUrl, connection.token) } }
                .onSuccess { state.providers = it }
                .onFailure { state.notice = nodeFailureMessage(context, it, R.string.error_node_action_failed) }
        }
    }
    LaunchedEffect(state.selectedBackupId, state.backups, state.providers) {
        val selectedBackup = state.backups.firstOrNull { it.backupId == state.selectedBackupId }
        if (state.selectedBackupId.isNotBlank() && selectedBackup == null) {
            state.selectedBackupId = ""
        } else if (selectedBackup != null && state.backupName.isBlank()) {
            state.backupName = selectedBackup.name
            val eligible = state.providers.filter(::isProviderEligibleForBackup).map(Provider::peerId).toSet()
            state.selectedProviderIds = selectedBackup.selectedProviderIds.intersect(eligible)
        }
    }
    FormPage(
        modifier,
        stringResource(R.string.backup_title),
        stringResource(R.string.backup_subtitle),
    ) {
        SectionTitle(stringResource(R.string.backup_mode))
        FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            FilterChip(
                selected = state.selectedBackupId.isBlank(),
                onClick = {
                    state.selectedBackupId = ""
                    state.selectedProviderIds = emptySet()
                },
                label = { Text(stringResource(R.string.backup_create_new)) },
            )
            state.backups.forEach { backup ->
                FilterChip(
                    selected = state.selectedBackupId == backup.backupId,
                    onClick = {
                        state.selectedBackupId = backup.backupId
                        state.backupName = backup.name
                        val eligible = state.providers.filter(::isProviderEligibleForBackup).map(Provider::peerId).toSet()
                        state.selectedProviderIds = backup.selectedProviderIds.intersect(eligible)
                    },
                    label = { Text(stringResource(R.string.backup_add_snapshot, backup.name)) },
                )
            }
        }
        OutlinedButton(onClick = pickSource) {
            Icon(Icons.Rounded.FolderOpen, contentDescription = null)
            Text(stringResource(R.string.action_choose_source), Modifier.padding(start = 8.dp))
        }
        Text(
            state.selectedSource?.let { DocumentFile.fromTreeUri(context, it)?.name ?: it.toString() }
                ?: stringResource(R.string.no_source_selected),
        )
        OutlinedTextField(
            state.backupName,
            { state.backupName = it },
            label = { Text(stringResource(R.string.field_backup_name)) },
            singleLine = true,
            modifier = Modifier.fillMaxWidth(),
        )
        SectionTitle(stringResource(R.string.extra_copies))
        Text(
            stringResource(R.string.extra_copies_detail),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        if (state.providers.isEmpty()) {
            EmptyState(
                stringResource(R.string.no_providers_title),
                stringResource(R.string.no_providers_detail),
            )
        }
        state.providers.forEach { provider ->
            ProviderSelectionRow(
                provider,
                selected = provider.peerId in state.selectedProviderIds,
                enabled = isProviderEligibleForBackup(provider),
            ) { checked ->
                state.selectedProviderIds = if (checked) {
                    state.selectedProviderIds + provider.peerId
                } else {
                    state.selectedProviderIds - provider.peerId
                }
            }
        }
        BackupPreflight(
            sourceName = state.selectedSource?.let {
                DocumentFile.fromTreeUri(context, it)?.name ?: it.toString()
            },
            selectedProviders = state.providers.filter { it.peerId in state.selectedProviderIds },
            previousProviderIds = state.backups
                .firstOrNull { it.backupId == state.selectedBackupId }
                ?.selectedProviderIds
                .orEmpty(),
        )
        FilledTonalButton(
            enabled = state.selectedSource != null && state.backupName.isNotBlank() && !state.busy &&
                connection != null && state.connectionHealth == ConnectionHealth.READY &&
                state.selectedProviderIds.all { selectedId ->
                    state.providers.any { it.peerId == selectedId && isProviderEligibleForBackup(it) }
            },
            onClick = {
                connection ?: return@FilledTonalButton
                val source = state.selectedSource ?: return@FilledTonalButton
                api(context, state, scope) {
                    check(state.selectedProviderIds.all { selectedId ->
                        state.providers.any { it.peerId == selectedId && isProviderEligibleForBackup(it) }
                    }) { resources.getString(R.string.provider_capacity_required) }
                    ensureReadableSource(context, source)
                    val jobId = newId("backup")
                    val payload = JSONObject()
                        .put("displayName", state.backupName.trim())
                        .put("snapshotId", newId("snapshot"))
                        .put("jobId", jobId)
                        .put("selectedProviderIds", JSONArray(state.selectedProviderIds.sorted()))
                    if (state.selectedBackupId.isNotBlank()) {
                        payload.put("backupId", state.selectedBackupId)
                    }
                    queueTransfer(
                        context,
                        store,
                        TransferRecord(
                            jobId = jobId,
                            label = state.backupName.trim(),
                            kind = TransferKind.BACKUP,
                            state = TransferState.QUEUED,
                            detail = resources.getString(R.string.transfer_queued_detail),
                        ),
                        "/api/v1/backups/archive",
                        payload,
                        TransferWorker.MODE_SAF_BACKUP,
                        source,
                    )
                    state.refreshDurableState()
                    state.notice = resources.getString(R.string.backup_queued)
                    state.backupName = ""
                    state.selectedBackupId = ""
                    state.selectedProviderIds = emptySet()
                    state.screen = Screen.HOME
                }
            },
        ) {
            Icon(Icons.Rounded.Backup, contentDescription = null)
            Text(stringResource(R.string.action_queue_backup), Modifier.padding(start = 8.dp))
        }
    }
}

@Composable
private fun BackupPreflight(
    sourceName: String?,
    selectedProviders: List<Provider>,
    previousProviderIds: Set<String>,
) {
    val selectedIds = selectedProviders.map(Provider::peerId).toSet()
    val added = selectedIds - previousProviderIds
    val removed = previousProviderIds - selectedIds
    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(7.dp)) {
            Text(
                stringResource(R.string.backup_review_title),
                style = MaterialTheme.typography.titleMedium,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier.semantics { heading() },
            )
            Text(stringResource(R.string.backup_review_source, sourceName ?: stringResource(R.string.no_source_selected)))
            Text(stringResource(R.string.backup_review_exclusions))
            Text(stringResource(R.string.backup_review_access))
            Text(
                if (selectedProviders.isEmpty()) {
                    stringResource(R.string.backup_review_local_only)
                } else {
                    pluralStringResource(
                        R.plurals.backup_review_copy_count,
                        selectedProviders.size,
                        selectedProviders.size,
                    )
                },
            )
            selectedProviders.forEach { provider ->
                Text("• ${provider.displayName ?: provider.address}")
            }
            Text(
                stringResource(R.string.backup_review_readability),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            if (previousProviderIds.isNotEmpty()) {
                HorizontalDivider()
                Text(
                    stringResource(R.string.replica_change_existing_impact),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                when {
                    added.isEmpty() && removed.isEmpty() -> Text(stringResource(R.string.replica_set_unchanged))
                    else -> {
                        if (added.isNotEmpty()) Text(pluralStringResource(R.plurals.replica_add_impact, added.size, added.size))
                        if (removed.isNotEmpty()) Text(pluralStringResource(R.plurals.replica_remove_impact, removed.size, removed.size))
                    }
                }
            }
        }
    }
}

internal fun isProviderEligibleForBackup(provider: Provider): Boolean =
    provider.reachability == ProviderReachability.REACHABLE &&
        provider.capacityBytes != null && provider.capacityBytes > 0 &&
        provider.quotaBytes != null && provider.quotaBytes > 0 &&
        provider.observedAtUnixMs != null && provider.validUntilUnixMs != null &&
        provider.validUntilUnixMs >= provider.observedAtUnixMs &&
        provider.validUntilUnixMs >= System.currentTimeMillis()

@Composable
private fun ProviderSelectionRow(
    provider: Provider,
    selected: Boolean,
    enabled: Boolean,
    onSelected: (Boolean) -> Unit,
) {
    val title = provider.displayName ?: stringResource(R.string.provider_unnamed)
    val reachability = when (provider.reachability) {
        ProviderReachability.REACHABLE -> stringResource(R.string.provider_connected_state)
        ProviderReachability.UNREACHABLE -> stringResource(R.string.provider_offline_state)
        ProviderReachability.UNKNOWN -> stringResource(R.string.provider_unknown_state)
    }
    val roles = provider.roles.sorted().joinToString(", ").ifBlank { stringResource(R.string.provider_roles_unavailable) }
    OutlinedCard(
        Modifier
            .fillMaxWidth()
            .toggleable(
                value = selected,
                enabled = enabled,
                role = Role.Checkbox,
                onValueChange = onSelected,
            )
            .semantics(mergeDescendants = true) {},
    ) {
        Row(Modifier.padding(14.dp), verticalAlignment = Alignment.CenterVertically) {
            Checkbox(selected, onCheckedChange = null, enabled = enabled)
            Column(Modifier.padding(start = 10.dp).weight(1f)) {
                Text(title, fontWeight = FontWeight.SemiBold)
                Text(provider.address)
                Text(
                    stringResource(R.string.provider_metadata, reachability, roles),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Text(
                    stringResource(R.string.provider_fingerprint, shortFingerprint(provider.fingerprint)),
                    style = MaterialTheme.typography.bodySmall,
                    fontFamily = FontFamily.Monospace,
                )
                Text(
                    when {
                        provider.reachability == ProviderReachability.UNREACHABLE ->
                            stringResource(R.string.provider_capacity_offline)
                        provider.reachability == ProviderReachability.UNKNOWN ->
                            stringResource(R.string.provider_reachability_unverified)
                        provider.capacityBytes == null || provider.allocatedBytes == null ||
                            provider.quotaBytes == null || provider.validUntilUnixMs == null ->
                            stringResource(R.string.provider_capacity_unavailable)
                        provider.capacityBytes <= 0 ->
                            stringResource(R.string.provider_capacity_insufficient)
                        else -> stringResource(
                            R.string.provider_capacity_available,
                            Formatter.formatFileSize(LocalContext.current, provider.capacityBytes),
                            Formatter.formatFileSize(LocalContext.current, provider.allocatedBytes),
                            Formatter.formatFileSize(LocalContext.current, provider.quotaBytes),
                        )
                    },
                    style = MaterialTheme.typography.bodySmall,
                    color = if (enabled) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.error,
                )
            }
        }
    }
}

@Composable
private fun Restore(
    state: CovalentViewModel,
    node: CovalentNodeClient,
    store: SecureNodeStore,
    connection: NodeConnection?,
    scope: kotlinx.coroutines.CoroutineScope,
    modifier: Modifier,
    pickTarget: () -> Unit,
) {
    val context = LocalContext.current
    val resources = LocalResources.current
    val available = state.backups.filter { it.latestSnapshotId != null }
    LaunchedEffect(available.map(RememberedBackup::backupId)) {
        if (available.none { it.backupId == state.selectedRestoreBackupId }) {
            state.selectedRestoreBackupId = available.firstOrNull()?.backupId.orEmpty()
            state.persistRestorePlan(null)
        }
    }
    val selected = available.firstOrNull { it.backupId == state.selectedRestoreBackupId }
    fun queueRestore(plan: RestorePlanPage) {
        val target = state.selectedTarget ?: return
        api(context, state, scope) {
            ensureWritableTarget(context, target)
            val reference = plan.reference
            queueTransfer(
                context,
                store,
                TransferRecord(
                    jobId = reference.jobId,
                    label = selected?.name ?: resources.getString(R.string.restore_title),
                    kind = TransferKind.RESTORE,
                    state = TransferState.QUEUED,
                    detail = resources.getString(R.string.transfer_queued_detail),
                    totalEntries = reference.totalEntries,
                ),
                "/api/v1/restores/archive/execute",
                restoreTransferPayload(reference),
                TransferWorker.MODE_SAF_RESTORE,
                target,
            )
            state.persistRestorePlan(null)
            state.refreshDurableState()
            state.notice = resources.getString(R.string.restore_queued)
            state.screen = Screen.HOME
        }
    }
    FormPage(
        modifier,
        stringResource(R.string.restore_title),
        stringResource(R.string.restore_subtitle),
    ) {
        SectionTitle(stringResource(R.string.restore_choose_backup))
        if (available.isEmpty()) {
            EmptyState(
                stringResource(R.string.restore_no_backups_title),
                stringResource(R.string.restore_no_backups_detail),
            )
        }
        available.forEach { backup ->
            val chosen = backup.backupId == state.selectedRestoreBackupId
            OutlinedCard(
                Modifier
                    .fillMaxWidth()
                    .toggleable(value = chosen, role = Role.RadioButton) {
                        state.selectedRestoreBackupId = backup.backupId
                        state.persistRestorePlan(null)
                    }
                    .semantics(mergeDescendants = true) {},
            ) {
                Row(Modifier.padding(14.dp), verticalAlignment = Alignment.CenterVertically) {
                    androidx.compose.material3.RadioButton(chosen, onClick = null)
                    Column(Modifier.padding(start = 10.dp)) {
                        Text(backup.name, fontWeight = FontWeight.SemiBold)
                        Text(
                            pluralStringResource(
                                R.plurals.restore_latest_snapshot,
                                backup.snapshotCount.toInt(),
                                backup.snapshotCount,
                            ),
                            style = MaterialTheme.typography.bodySmall,
                        )
                    }
                }
            }
        }
        SectionTitle(stringResource(R.string.restore_conflict_title))
        RestoreConflictPolicy.entries.forEach { policy ->
            val selectedPolicy = state.selectedRestorePolicy == policy
            val title = when (policy) {
                RestoreConflictPolicy.FAIL -> stringResource(R.string.restore_policy_fail)
                RestoreConflictPolicy.SKIP -> stringResource(R.string.restore_policy_skip)
                RestoreConflictPolicy.RENAME -> stringResource(R.string.restore_policy_rename)
            }
            val detail = when (policy) {
                RestoreConflictPolicy.FAIL -> stringResource(R.string.restore_policy_fail_detail)
                RestoreConflictPolicy.SKIP -> stringResource(R.string.restore_policy_skip_detail)
                RestoreConflictPolicy.RENAME -> stringResource(R.string.restore_policy_rename_detail)
            }
            OutlinedCard(
                Modifier
                    .fillMaxWidth()
                    .toggleable(value = selectedPolicy, role = Role.RadioButton) {
                        state.selectedRestorePolicy = policy
                        state.persistRestorePlan(null)
                    }
                    .semantics(mergeDescendants = true) {}
                    .testTag("restore.policy.${policy.wireValue}"),
            ) {
                Row(Modifier.padding(14.dp), verticalAlignment = Alignment.CenterVertically) {
                    androidx.compose.material3.RadioButton(selectedPolicy, onClick = null)
                    Column(Modifier.padding(start = 10.dp)) {
                        Text(title, fontWeight = FontWeight.SemiBold)
                        Text(
                            detail,
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
            }
        }
        Text(
            stringResource(R.string.restore_policy_replace_unavailable),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        OutlinedButton(onClick = pickTarget) {
            Icon(Icons.Rounded.FolderOpen, contentDescription = null)
            Text(stringResource(R.string.action_choose_restore_folder), Modifier.padding(start = 8.dp))
        }
        Text(
            state.selectedTarget?.let { DocumentFile.fromTreeUri(context, it)?.name ?: it.toString() }
                ?: stringResource(R.string.no_restore_target),
        )
        OutlinedButton(
            enabled = selected != null && state.selectedTarget != null && !state.busy &&
                connection != null && state.connectionHealth == ConnectionHealth.READY,
            onClick = {
                val selectedConnection = connection ?: return@OutlinedButton
                val target = state.selectedTarget ?: return@OutlinedButton
                val backup = selected ?: return@OutlinedButton
                api(context, state, scope) {
                    ensureWritableTarget(context, target)
                    val jobId = newId("restore")
                    val inventory = SafTransferBridge(node).targetInventory(context, target)
                    val uploaded = node.uploadTargetInventory(
                        selectedConnection.baseUrl,
                        selectedConnection.token,
                        jobId,
                        inventory,
                    )
                    state.persistRestorePlan(
                        node.previewArchiveRestore(
                            selectedConnection.baseUrl,
                            selectedConnection.token,
                            JSONObject()
                                .put("backupId", backup.backupId)
                                .put("snapshotId", backup.latestSnapshotId)
                                .put("conflictPolicy", state.selectedRestorePolicy.wireValue)
                                .put("jobId", jobId)
                                .put("targetInventoryId", uploaded.inventoryId),
                        ),
                    )
                }
            },
        ) { Text(stringResource(R.string.action_preview_restore)) }
        state.restorePlan?.let { plan ->
            val selectedConnection = connection
            RestorePlanPreview(
                plan = plan,
                busy = state.busy,
                onNextPage = selectedConnection?.let { active -> plan.nextCursor?.let { cursor ->
                    {
                        api(context, state, scope) {
                            state.persistRestorePlan(
                                node.restorePlanPage(
                                    active.baseUrl,
                                    active.token,
                                    plan.reference,
                                    cursor,
                                ),
                            )
                        }
                    }
                } },
            )
            FilledTonalButton(
                enabled = connection != null && !state.busy && state.connectionHealth == ConnectionHealth.READY &&
                    plan.reference.targetInventory != null &&
                    plan.reference.conflictPolicy == state.selectedRestorePolicy.wireValue &&
                    plan.entries.all { isSafeSafRestoreAction(it.kind, it.action) },
                onClick = { queueRestore(plan) },
            ) {
                Icon(Icons.Rounded.FolderOpen, contentDescription = null)
                Text(stringResource(R.string.action_queue_restore), Modifier.padding(start = 8.dp))
            }
        }
    }
}

private fun safeRestoreActionLabel(kind: String, action: String): String =
    if (isSafeSafRestoreAction(kind, action)) action else "blocked_conflict"

@Composable
private fun RestorePlanPreview(plan: RestorePlanPage, busy: Boolean, onNextPage: (() -> Unit)?) {
    val pageStart = plan.entryOffset + 1
    val pageEnd = plan.entryOffset + plan.entries.size
    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(stringResource(R.string.restore_signed_preview), style = MaterialTheme.typography.titleMedium)
            Text(
                pluralStringResource(
                    R.plurals.restore_change_count,
                    plan.reference.totalEntries.toInt(),
                    plan.reference.totalEntries,
                ),
            )
            if (plan.entries.isNotEmpty()) {
                Text(
                    stringResource(R.string.restore_preview_range, pageStart, pageEnd),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            Text(
                stringResource(R.string.restore_preview_policy),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            plan.entries.forEach { entry ->
                Row(horizontalArrangement = Arrangement.spacedBy(10.dp), verticalAlignment = Alignment.Top) {
                    Icon(
                        if (entry.kind == "directory") Icons.Rounded.FolderOpen else Icons.Rounded.Storage,
                        contentDescription = null,
                    )
                    Column(Modifier.weight(1f)) {
                        Text(entry.destinationPath, fontFamily = FontFamily.Monospace)
                        Text(
                            stringResource(
                                R.string.restore_entry_metadata,
                                entry.kind,
                                safeRestoreActionLabel(entry.kind, entry.action),
                            ),
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
            }
            if (onNextPage != null) {
                OutlinedButton(enabled = !busy, onClick = onNextPage) {
                    Text(stringResource(R.string.action_next_restore_paths))
                }
            }
        }
    }
}

@Composable
private fun Settings(
    state: CovalentViewModel,
    node: CovalentNodeClient,
    store: SecureNodeStore,
    connection: NodeConnection?,
    embeddedProvider: EmbeddedProviderState,
    activeMode: NodeMode,
    scope: kotlinx.coroutines.CoroutineScope,
    modifier: Modifier,
    export: (String) -> Unit,
    import: (Array<String>) -> Unit,
    onLanChange: (Boolean) -> Unit,
    onSelectExternal: () -> Unit,
    onSelectLocal: () -> Unit,
    onTogglePhoneProvider: (Boolean) -> Unit,
) {
    val context = LocalContext.current
    val resources = LocalResources.current
    val lanEnabled = state.status?.lanDiscovery == true
    var confirmImport by remember { mutableStateOf(false) }
    FormPage(
        modifier,
        stringResource(R.string.settings_title),
        stringResource(R.string.settings_subtitle),
    ) {
        SectionTitle(stringResource(R.string.node_mode_title))
        NodeModeChoice(
            title = stringResource(R.string.node_mode_external),
            detail = stringResource(R.string.node_mode_external_detail),
            selected = activeMode == NodeMode.EXTERNAL,
            enabled = !state.busy,
            testTag = "settings.mode.external",
            onClick = onSelectExternal,
        )
        NodeModeChoice(
            title = stringResource(R.string.node_mode_phone),
            detail = stringResource(R.string.node_mode_phone_detail),
            selected = activeMode == NodeMode.LOCAL,
            enabled = !state.busy && embeddedProvider.enabled && embeddedProvider.running &&
                embeddedProvider.keyProtectionAvailable,
            testTag = "settings.mode.phone",
            onClick = onSelectLocal,
        )
        PhoneProviderSettings(
            state = state,
            provider = embeddedProvider,
            onToggle = onTogglePhoneProvider,
        )
        HorizontalDivider()
        val lanDescription = stringResource(if (lanEnabled) R.string.covalent_state_on else R.string.covalent_state_off)
        Row(
            Modifier
                .fillMaxWidth()
                .toggleable(
                    value = lanEnabled,
                    enabled = !state.busy && state.connectionHealth == ConnectionHealth.READY,
                    role = Role.Switch,
                    onValueChange = onLanChange,
                )
                .semantics(mergeDescendants = true) { stateDescription = lanDescription }
                .padding(vertical = 10.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Column(Modifier.weight(1f)) {
                Text(stringResource(R.string.lan_discovery), style = MaterialTheme.typography.titleMedium)
                Text(
                    stringResource(R.string.lan_discovery_detail),
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            Switch(
                lanEnabled,
                onCheckedChange = null,
                modifier = Modifier.clearAndSetSemantics {},
            )
        }
        OutlinedButton(enabled = !state.busy, onClick = { export("covalent-settings.json") }) {
            Text(stringResource(R.string.action_export_settings))
        }
        OutlinedButton(enabled = !state.busy, onClick = {
            state.setImportCandidate(null)
            import(arrayOf("application/json", "text/json"))
        }) { Text(stringResource(R.string.action_import_settings)) }
        state.importCandidate?.let { candidate ->
            val preview = settingsPreview(state.currentExportedSettings, candidate)
            val removedBackups = (preview.oldBackups - preview.newBackups).coerceAtLeast(0)
            fun applyImport() {
                val selectedConnection = connection ?: return
                api(context, state, scope) {
                    node.postNoContent(
                        selectedConnection.baseUrl,
                        selectedConnection.token,
                        "/api/v1/config/import",
                        JSONObject().put("confirmed", true).put("settings", candidate),
                    )
                    state.setImportCandidate(null)
                    refreshStatus(state, node, store, selectedConnection)
                    state.notice = resources.getString(R.string.settings_imported)
                }
            }
            SettingsImportPreview(preview)
            if (confirmImport) {
                DestructiveConfirmDialog(
                    action = DestructiveAction.IMPORT_REMOVING_BACKUPS,
                    subject = pluralStringResource(
                        R.plurals.settings_removed_backup_count,
                        removedBackups,
                        removedBackups,
                    ),
                    onConfirm = {
                        confirmImport = false
                        applyImport()
                    },
                    onDismiss = { confirmImport = false },
                )
            }
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                FilledTonalButton(
                    enabled = !state.busy && connection != null,
                    onClick = {
                        if (preview.removesBackups) confirmImport = true else applyImport()
                    },
                    modifier = Modifier.testTag("settings.import.confirm"),
                ) { Text(stringResource(R.string.action_confirm_import)) }
                TextButton(onClick = {
                    confirmImport = false
                    state.setImportCandidate(null)
                }) {
                    Text(stringResource(R.string.action_cancel))
                }
            }
        }
        OutlinedButton(onClick = {
            onSelectExternal()
            state.screen = Screen.SETUP
        }) {
            Text(stringResource(R.string.action_change_connection))
        }
        Text(
            stringResource(R.string.folder_revocation_policy),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

@Composable
private fun NodeModeChoice(
    title: String,
    detail: String,
    selected: Boolean,
    enabled: Boolean,
    testTag: String,
    onClick: () -> Unit,
) {
    OutlinedCard(
        Modifier
            .fillMaxWidth()
            .toggleable(
                value = selected,
                enabled = enabled,
                role = Role.RadioButton,
                onValueChange = { onClick() },
            )
            .testTag(testTag)
            .semantics(mergeDescendants = true) {},
    ) {
        Row(Modifier.padding(14.dp), verticalAlignment = Alignment.CenterVertically) {
            androidx.compose.material3.RadioButton(selected = selected, onClick = null, enabled = enabled)
            Column(Modifier.padding(start = 10.dp).weight(1f)) {
                Text(title, fontWeight = FontWeight.SemiBold)
                Text(
                    detail,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}

/**
 * Copy for the measured Android Keystore protection level, or null when the device cannot
 * protect its identity at all — that case is covered by the blocking message below.
 */
internal fun keyProtectionCopyRes(level: KeyProtectionLevel): Int? = when (level) {
    KeyProtectionLevel.UNAVAILABLE -> null
    KeyProtectionLevel.SOFTWARE -> R.string.phone_provider_protection_software
    KeyProtectionLevel.TRUSTED_ENVIRONMENT -> R.string.phone_provider_protection_hardware
    KeyProtectionLevel.STRONGBOX -> R.string.phone_provider_protection_strongbox
}

@Composable
private fun PhoneProviderSettings(
    state: CovalentViewModel,
    provider: EmbeddedProviderState,
    onToggle: (Boolean) -> Unit,
) {
    val context = LocalContext.current
    val capacity = providerCapacityBytes(state.providerMaximumGiB, state.providerKeepFreeGiB)
    val canConfigure = provider.supported && provider.keyProtectionAvailable && !state.busy && !provider.enabled
    var confirmDisable by remember { mutableStateOf(false) }
    if (confirmDisable) {
        DestructiveConfirmDialog(
            action = DestructiveAction.DISABLE_PHONE_STORAGE,
            onConfirm = {
                confirmDisable = false
                onToggle(false)
            },
            onDismiss = { confirmDisable = false },
        )
    }
    SectionTitle(stringResource(R.string.phone_provider_title))
    Text(
        stringResource(R.string.phone_provider_detail),
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
    OutlinedCard(Modifier.fillMaxWidth().testTag("settings.phoneProvider.status")) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(7.dp)) {
            Text(stringResource(R.string.phone_provider_status), style = MaterialTheme.typography.titleMedium)
            Text(provider.statusMessage)
            Text(
                stringResource(
                    R.string.phone_provider_storage,
                    Formatter.formatFileSize(context, provider.usedBytes),
                    Formatter.formatFileSize(context, provider.reservedBytes),
                    Formatter.formatFileSize(context, provider.availableBytes),
                ),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            // Say which protection this phone actually measured, rather than implying
            // hardware backing everywhere. A software-only device is allowed to store
            // backups and is told plainly that it is the weaker case.
            keyProtectionCopyRes(provider.keyProtectionLevel)?.let { copy ->
                Text(
                    stringResource(copy),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.testTag("settings.phoneProvider.protection"),
                )
            }
        }
    }
    when {
        !provider.keyProtectionAvailable -> Text(
            stringResource(R.string.phone_provider_locked),
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.testTag("settings.phoneProvider.locked"),
        )
        !provider.supported -> Text(
            stringResource(R.string.phone_provider_runtime_missing),
            color = MaterialTheme.colorScheme.error,
        )
        else -> {
            OutlinedTextField(
                value = state.providerMaximumGiB,
                onValueChange = { state.providerMaximumGiB = it },
                enabled = canConfigure,
                isError = capacity == null,
                label = { Text(stringResource(R.string.phone_provider_use_up_to)) },
                singleLine = true,
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Decimal, imeAction = ImeAction.Next),
                modifier = Modifier.fillMaxWidth().testTag("settings.phoneProvider.maximum"),
            )
            OutlinedTextField(
                value = state.providerKeepFreeGiB,
                onValueChange = { state.providerKeepFreeGiB = it },
                enabled = canConfigure,
                isError = capacity == null,
                label = { Text(stringResource(R.string.phone_provider_keep_free)) },
                supportingText = if (capacity == null) {
                    { Text(stringResource(R.string.phone_provider_invalid_capacity)) }
                } else null,
                singleLine = true,
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Decimal, imeAction = ImeAction.Done),
                modifier = Modifier.fillMaxWidth().testTag("settings.phoneProvider.keepFree"),
            )
            val lanDescription = stringResource(
                if (state.providerLanDiscovery) R.string.covalent_state_on else R.string.covalent_state_off,
            )
            Row(
                Modifier
                    .fillMaxWidth()
                    .toggleable(
                        value = state.providerLanDiscovery,
                        enabled = canConfigure,
                        role = Role.Switch,
                        onValueChange = { state.providerLanDiscovery = it },
                    )
                    .semantics(mergeDescendants = true) { stateDescription = lanDescription }
                    .testTag("settings.phoneProvider.lan")
                    .padding(vertical = 8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Column(Modifier.weight(1f)) {
                    Text(stringResource(R.string.phone_provider_lan), style = MaterialTheme.typography.titleMedium)
                    Text(
                        stringResource(R.string.phone_provider_lan_detail),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                Switch(
                    checked = state.providerLanDiscovery,
                    onCheckedChange = null,
                    enabled = canConfigure,
                    modifier = Modifier.clearAndSetSemantics {},
                )
            }
            FilledTonalButton(
                enabled = !state.busy && (provider.enabled || capacity != null),
                onClick = { if (provider.enabled) confirmDisable = true else onToggle(true) },
                modifier = Modifier.testTag("settings.phoneProvider.toggle"),
            ) {
                Text(
                    stringResource(
                        if (provider.enabled) R.string.action_disable_phone_provider
                        else R.string.action_enable_phone_provider,
                    ),
                )
            }
        }
    }
}

internal data class SettingsPreview(
    val oldName: String,
    val newName: String,
    val oldLan: Boolean,
    val newLan: Boolean,
    val oldBackups: Int,
    val newBackups: Int,
) {
    val removesBackups: Boolean get() = newBackups < oldBackups
}

internal fun settingsPreview(current: JSONObject?, candidate: JSONObject): SettingsPreview = SettingsPreview(
    oldName = current?.optString("deviceName").orEmpty(),
    newName = candidate.getString("deviceName"),
    oldLan = current?.optBoolean("lanDiscoveryEnabled") ?: false,
    newLan = candidate.getBoolean("lanDiscoveryEnabled"),
    oldBackups = current?.optJSONArray("rememberedBackups")?.length() ?: 0,
    newBackups = candidate.getJSONArray("rememberedBackups").length(),
)

private fun validateSettingsCandidate(context: Context, candidate: JSONObject) {
    check(candidate.getInt("schemaVersion") == 1) {
        context.getString(R.string.error_settings_schema_unsupported)
    }
    check(candidate.getString("deviceName").isNotBlank()) {
        context.getString(R.string.error_settings_device_name_missing)
    }
    candidate.getBoolean("lanDiscoveryEnabled")
    candidate.getJSONArray("rememberedBackups")
}

@Composable
private fun SettingsImportPreview(preview: SettingsPreview) {
    val none = stringResource(R.string.value_none)
    OutlinedCard(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            Text(stringResource(R.string.settings_import_preview), style = MaterialTheme.typography.titleMedium)
            Text(stringResource(R.string.settings_name_change, preview.oldName.ifBlank { none }, preview.newName))
            Text(
                stringResource(
                    R.string.settings_lan_change,
                    stringResource(if (preview.oldLan) R.string.covalent_state_on else R.string.covalent_state_off),
                    stringResource(if (preview.newLan) R.string.covalent_state_on else R.string.covalent_state_off),
                ),
            )
            Text(stringResource(R.string.settings_backup_change, preview.oldBackups, preview.newBackups))
            if (preview.removesBackups) InlineError(stringResource(R.string.settings_backup_removal_warning))
            Text(
                stringResource(R.string.settings_import_keys_excluded),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

@Composable
private fun FormPage(modifier: Modifier, title: String, subtitle: String, content: @Composable () -> Unit) {
    LazyColumn(
        modifier,
        contentPadding = PaddingValues(20.dp, 14.dp, 20.dp, 32.dp),
        verticalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        item {
            Text(
                title,
                style = MaterialTheme.typography.headlineMedium,
                fontWeight = FontWeight.SemiBold,
                modifier = Modifier.semantics { heading() },
            )
        }
        item { Text(subtitle, color = MaterialTheme.colorScheme.onSurfaceVariant) }
        item { Column(verticalArrangement = Arrangement.spacedBy(14.dp), content = { content() }) }
    }
}

@Composable
private fun StatusCard(title: String, value: String, detail: String, icon: ImageVector) {
    Card(Modifier.fillMaxWidth()) {
        Row(
            Modifier.padding(18.dp),
            horizontalArrangement = Arrangement.spacedBy(14.dp),
            verticalAlignment = Alignment.Top,
        ) {
            Surface(shape = MaterialTheme.shapes.medium, color = MaterialTheme.colorScheme.secondaryContainer) {
                Icon(icon, contentDescription = null, Modifier.padding(12.dp))
            }
            Column {
                Text(title, style = MaterialTheme.typography.labelLarge)
                Text(value, style = MaterialTheme.typography.titleLarge, fontWeight = FontWeight.SemiBold)
                Spacer(Modifier.height(5.dp))
                Text(detail, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
        }
    }
}

@Composable
private fun EmptyState(title: String, detail: String) {
    Card(Modifier.fillMaxWidth()) {
        Row(Modifier.padding(18.dp), horizontalArrangement = Arrangement.spacedBy(14.dp)) {
            Icon(Icons.Rounded.CloudOff, contentDescription = null)
            Column {
                Text(title, fontWeight = FontWeight.SemiBold)
                Text(detail, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
        }
    }
}

@Composable
private fun InlineError(message: String) {
    Row(
        Modifier.fillMaxWidth().semantics { contentDescription = message },
        horizontalArrangement = Arrangement.spacedBy(8.dp),
        verticalAlignment = Alignment.Top,
    ) {
        Icon(Icons.Rounded.ErrorOutline, contentDescription = null, tint = MaterialTheme.colorScheme.error)
        Text(message, color = MaterialTheme.colorScheme.error, modifier = Modifier.weight(1f))
    }
}

@Composable
private fun SectionTitle(title: String, modifier: Modifier = Modifier) {
    Text(
        title,
        style = MaterialTheme.typography.titleLarge,
        modifier = modifier.semantics { heading() },
    )
}

@Composable
fun PrimaryActionToolbar(
    enabled: Boolean,
    compact: Boolean = false,
    onAction: (PrimaryAction) -> Unit,
) {
    val description = stringResource(R.string.primary_actions)
    BoxWithConstraints {
        val iconOnly = compact || maxWidth < 340.dp
        Surface(
            modifier = Modifier
                .semantics { contentDescription = description }
                .padding(horizontal = 12.dp),
            shape = MaterialTheme.shapes.extraLarge,
            color = MaterialTheme.colorScheme.surfaceContainerHigh,
            tonalElevation = 6.dp,
            shadowElevation = 3.dp,
        ) {
            Row(
                Modifier.padding(8.dp),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                ToolbarButton(PrimaryAction.PAIR, enabled, iconOnly, onAction, Icons.Rounded.AddLink)
                ToolbarButton(PrimaryAction.BACKUP, enabled, iconOnly, onAction, Icons.Rounded.Backup)
                ToolbarButton(PrimaryAction.RESTORE, enabled, iconOnly, onAction, Icons.Rounded.FolderOpen)
            }
        }
    }
}

@Composable
private fun ToolbarButton(
    action: PrimaryAction,
    enabled: Boolean,
    iconOnly: Boolean,
    onAction: (PrimaryAction) -> Unit,
    icon: ImageVector,
) {
    val label = when (action) {
        PrimaryAction.PAIR -> stringResource(R.string.action_pair)
        PrimaryAction.BACKUP -> stringResource(R.string.action_backup)
        PrimaryAction.RESTORE -> stringResource(R.string.action_restore)
    }
    if (iconOnly) {
        IconButton(onClick = { onAction(action) }, enabled = enabled) {
            Icon(icon, contentDescription = label)
        }
    } else {
        FilledTonalButton(
            onClick = { onAction(action) },
            enabled = enabled,
            contentPadding = PaddingValues(horizontal = 14.dp, vertical = 12.dp),
        ) {
            Icon(icon, contentDescription = null)
            Text(label, Modifier.padding(start = 7.dp))
        }
    }
}

private fun api(
    context: Context,
    state: CovalentViewModel,
    scope: kotlinx.coroutines.CoroutineScope,
    onError: ((Throwable) -> Unit)? = null,
    pairingError: Boolean = false,
    work: suspend () -> Unit,
) {
    scope.launch {
        state.busy = true
        if (pairingError) state.pairingError = ""
        runCatching { withContext(Dispatchers.IO) { work() } }.onFailure { error ->
            when {
                onError != null -> onError(error)
                pairingError -> state.pairingError =
                    nodeFailureMessage(context, error, R.string.error_pairing_failed)
                else -> state.notice =
                    nodeFailureMessage(context, error, R.string.error_node_action_failed)
            }
        }
        state.busy = false
    }
}

private fun refreshStatus(
    state: CovalentViewModel,
    node: CovalentNodeClient,
    store: SecureNodeStore,
    connection: NodeConnection,
) {
    val status = node.status(connection.baseUrl)
    val backups = node.backups(connection.baseUrl, connection.token)
    state.status = status
    store.replaceBackups(backups)
    state.backups = backups
    state.providers = node.providers(connection.baseUrl, connection.token)
    state.transfers = store.transfers()
}

private fun reconnectNode(
    context: Context,
    state: CovalentViewModel,
    node: CovalentNodeClient,
    store: SecureNodeStore,
    connection: NodeConnection,
    scope: kotlinx.coroutines.CoroutineScope,
) {
    state.connectionHealth = ConnectionHealth.CONNECTING
    api(context, state, scope, onError = { error ->
        state.connectionHealth = ConnectionHealth.STALE
        state.connectionError = nodeFailureMessage(context, error, R.string.error_connection_failed)
        if (shouldReturnToSetupAfterRefreshFailure(error)) state.screen = Screen.SETUP
    }) {
        refreshStatus(state, node, store, connection)
        TransferExecution.reconcileAcknowledgements(store, node, connection)
        state.connectionHealth = ConnectionHealth.READY
        state.connectionError = null
        state.lastConnectedAtUnixMs = System.currentTimeMillis()
    }
}

private fun setLanDiscovery(
    context: Context,
    state: CovalentViewModel,
    node: CovalentNodeClient,
    store: SecureNodeStore,
    connection: NodeConnection,
    enabled: Boolean,
) {
    val settings = node.post(connection.baseUrl, connection.token, "/api/v1/config/export", JSONObject())
    settings.put("lanDiscoveryEnabled", enabled)
    node.postNoContent(
        connection.baseUrl,
        connection.token,
        "/api/v1/config/import",
        JSONObject().put("confirmed", true).put("settings", settings),
    )
    refreshStatus(state, node, store, connection)
    state.notice = if (enabled) {
        context.getString(R.string.lan_discovery_enabled)
    } else {
        context.getString(R.string.lan_discovery_disabled)
    }
}

private fun ensureReadableSource(context: Context, uri: Uri) {
    check(context.contentResolver.persistedUriPermissions.any { it.uri == uri && it.isReadPermission }) {
        context.getString(R.string.error_source_access_revoked)
    }
}

private fun ensureWritableTarget(context: Context, uri: Uri) {
    check(
        context.contentResolver.persistedUriPermissions.any {
            it.uri == uri && it.isReadPermission && it.isWritePermission
        },
    ) { context.getString(R.string.error_target_access_revoked) }
    val target = DocumentFile.fromTreeUri(context, uri)
    check(target != null && target.exists() && target.isDirectory) {
        context.getString(R.string.error_target_unavailable)
    }
}

private fun queueTransfer(
    context: Context,
    store: SecureNodeStore,
    record: TransferRecord,
    path: String,
    payload: JSONObject,
    mode: String = "json",
    treeUri: Uri? = null,
) {
    // This runs on Dispatchers.IO through [api]. The synchronous journal commit keeps the
    // exact request and its visible queue record recoverable before scheduling can send it.
    store.saveQueuedTransfer(record, path, payload, mode, treeUri?.toString())
    try {
        TransferScheduler.enqueue(context, record.jobId)
    } catch (error: Exception) {
        store.updateTransfer(record.jobId) {
            it.copy(
                state = TransferState.FAILED,
                detail = nodeFailureMessage(context, error, R.string.error_node_action_failed),
                retryable = true,
            )
        }
        throw error
    }
}

private fun parseBoundedJson(raw: String): JSONObject {
    val bytes = raw.trim().encodeToByteArray()
    check(bytes.isNotEmpty() && bytes.size <= 512 * 1_024) { "Pairing exchange size is invalid." }
    return JSONObject(bytes.decodeToString())
}

private fun requireSameInvitation(invitation: JSONObject, session: JSONObject) {
    val returned = session.getJSONObject("invitation")
    check(returned.getString("invitationId") == invitation.getString("invitationId")) {
        "The returned session belongs to a different invitation."
    }
}

private fun requireSameSession(current: JSONObject, updated: JSONObject) {
    requireSameInvitation(current.getJSONObject("invitation"), updated)
    check(updated.getString("responderDeviceId") == current.getString("responderDeviceId")) {
        "The returned session belongs to a different responder."
    }
    check(updated.getString("authenticationString") == current.getString("authenticationString")) {
        "The returned session changed the comparison code."
    }
}

internal fun isExpired(session: JSONObject, nowUnixMs: Long = System.currentTimeMillis()): Boolean =
    isExpiredInvitation(session.getJSONObject("invitation"), nowUnixMs)

internal fun isExpiredInvitation(invitation: JSONObject, nowUnixMs: Long = System.currentTimeMillis()): Boolean =
    invitation.optLong("expiresAtUnixMs", 0) <= nowUnixMs

private fun remainingMinutes(invitation: JSONObject): Long =
    ((invitation.optLong("expiresAtUnixMs") - System.currentTimeMillis()).coerceAtLeast(0) + 59_999) / 60_000

private fun roleSet(serialized: String): Set<String> = serialized.split(',').filter { it in ALL_PAIRING_ROLES }.toSet()

private fun jsonStrings(values: JSONArray): Set<String> = buildSet {
    repeat(values.length()) { add(values.getString(it)) }
}

internal fun validProviderSocketAddress(value: String): Boolean {
    val trimmed = value.trim()
    val (host, port) = when {
        trimmed.startsWith('[') && "]:" in trimmed -> {
            val close = trimmed.indexOf(']')
            if (close <= 1 || close != trimmed.lastIndexOf(']')) return false
            trimmed.substring(1, close) to trimmed.substring(close + 2).toIntOrNull()
        }
        trimmed.count { it == ':' } == 1 ->
            trimmed.substringBefore(':') to trimmed.substringAfter(':').toIntOrNull()
        else -> return false
    }
    if (port !in 1..65_535) return false
    if (':' in host) {
        if ('%' in host || host.any { !it.isDigit() && it.lowercaseChar() !in 'a'..'f' && it !in setOf(':', '.') }) {
            return false
        }
        return runCatching { InetAddress.getByName(host).hostAddress?.contains(':') == true }.getOrDefault(false)
    }
    val octets = host.split('.').mapNotNull(String::toIntOrNull)
    return octets.size == 4 && octets.all { it in 0..255 }
}

private fun shortFingerprint(value: String): String = if (value.length <= 20) value else {
    "${value.take(10)}…${value.takeLast(10)}"
}

private fun copyText(context: Context, label: String, value: String) {
    context.getSystemService(ClipboardManager::class.java).setPrimaryClip(ClipData.newPlainText(label, value))
}

private fun shareText(context: Context, title: String, value: String) {
    val intent = Intent(Intent.ACTION_SEND)
        .setType("text/plain")
        .putExtra(Intent.EXTRA_TEXT, value)
    context.startActivity(Intent.createChooser(intent, title))
}

@Preview(showBackground = true, widthDp = 420, heightDp = 820)
@Composable
private fun AppPreview() {
    CovalentTheme {
        CovalentApp()
    }
}
