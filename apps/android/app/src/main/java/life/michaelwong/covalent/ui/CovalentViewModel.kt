package life.michaelwong.covalent.ui

import android.net.Uri
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.lifecycle.SavedStateHandle
import androidx.lifecycle.ViewModel
import kotlin.properties.ReadWriteProperty
import kotlin.reflect.KProperty
import life.michaelwong.covalent.data.SecureNodeStore
import life.michaelwong.covalent.data.EnrolledTrust
import life.michaelwong.covalent.data.restorePlanPageFromPersistence
import life.michaelwong.covalent.data.toPersistenceJson
import life.michaelwong.covalent.model.DiscoveryCandidate
import life.michaelwong.covalent.model.NodeStatus
import life.michaelwong.covalent.model.NetworkPairing
import life.michaelwong.covalent.model.Provider
import life.michaelwong.covalent.model.RememberedBackup
import life.michaelwong.covalent.model.RestorePlanPage
import life.michaelwong.covalent.model.RestoreConflictPolicy
import life.michaelwong.covalent.model.TransferRecord
import org.json.JSONObject

internal enum class ConnectionHealth { CONNECTING, READY, STALE, DISCONNECTED }

internal enum class PairingRole { INVITER, RESPONDER }

/**
 * Owns every in-progress Android workflow. Small safe identifiers and content URIs use
 * SavedStateHandle; signed JSON exchanges stay encrypted in SecureNodeStore.
 */
internal class CovalentViewModel(private val savedStateHandle: SavedStateHandle) : ViewModel() {
    private var store: SecureNodeStore? = null

    private var screenName by savedString("screen", Screen.SETUP.name)
    var screen: Screen
        get() = runCatching { Screen.valueOf(screenName) }.getOrDefault(Screen.SETUP)
        set(value) { screenName = value.name }

    var setupName by savedString("setup_name", "")
    var setupAddress by savedString("setup_address", "")
    // Never place an uncommitted bearer token in the activity saved-state bundle.
    var setupToken by mutableStateOf("")
    var setupNameError by savedString("setup_name_error", "")
    var setupAddressError by savedString("setup_address_error", "")
    var setupTokenError by savedString("setup_token_error", "")
    var setupConnectionError by savedString("setup_connection_error", "")
    var setupCaCertificateDer by savedString("setup_ca_certificate", "")
    var setupCaCertificateLabel by savedString("setup_ca_label", "")
    var setupCertificatePin by savedString("setup_certificate_pin", "")
    var setupCertificatePinError by savedString("setup_certificate_pin_error", "")
    var localPermissionDenied by savedBoolean("local_permission_denied", false)
    var pendingPermissionSetup by savedBoolean("pending_permission_setup", false)
    var pendingPermissionLanEnable by savedBoolean("pending_permission_lan_enable", false)
    var pendingPermissionDiscovery by savedBoolean("pending_permission_discovery", false)
    var pendingProviderEnable by savedBoolean("pending_provider_enable", false)
    var providerMaximumGiB by savedString("provider_maximum_gib", "2")
    var providerKeepFreeGiB by savedString("provider_keep_free_gib", "0.5")
    var providerLanDiscovery by savedBoolean("provider_lan_discovery", false)
    private var providerDraftLoaded by savedBoolean("provider_draft_loaded", false)

    var selectedSourceText by savedString("selected_source", "")
    var selectedSource: Uri?
        get() = selectedSourceText.takeIf(String::isNotBlank)?.let(Uri::parse)
        set(value) { selectedSourceText = value?.toString().orEmpty() }
    var selectedTargetText by savedString("selected_target", "")
    var selectedTarget: Uri?
        get() = selectedTargetText.takeIf(String::isNotBlank)?.let(Uri::parse)
        set(value) { selectedTargetText = value?.toString().orEmpty() }
    var backupName by savedString("backup_name", "")
    var selectedBackupId by savedString("backup_existing_id", "")
    var selectedProviderIdsText by savedString("selected_provider_ids", "")
    var selectedProviderIds: Set<String>
        get() = selectedProviderIdsText.split(',').filter(String::isNotBlank).toSet()
        set(value) { selectedProviderIdsText = value.sorted().joinToString(",") }
    var selectedRestoreBackupId by savedString("restore_backup_id", "")
    private var selectedRestorePolicyName by savedString(
        "restore_conflict_policy",
        RestoreConflictPolicy.FAIL.name,
    )
    var selectedRestorePolicy: RestoreConflictPolicy
        get() = runCatching { RestoreConflictPolicy.valueOf(selectedRestorePolicyName) }
            .getOrDefault(RestoreConflictPolicy.FAIL)
        set(value) { selectedRestorePolicyName = value.name }

    private var pairingRoleName by savedString("pairing_role", PairingRole.INVITER.name)
    var pairingRole: PairingRole
        get() = runCatching { PairingRole.valueOf(pairingRoleName) }.getOrDefault(PairingRole.INVITER)
        set(value) { pairingRoleName = value.name }
    var pairingInput by savedString("pairing_input", "")
    var pairingDisplayName by savedString("pairing_display_name", "Android")
    var tailscaleCandidateAddress by savedString("tailscale_candidate_address", "")
    var showAdvancedPairing by savedBoolean("show_advanced_pairing", false)
    var responderRolesText by savedString("responder_roles", "backup_writer")
    var inviterRolesText by savedString("inviter_roles", "storage_provider")
    var pairingError by savedString("pairing_error", "")

    var status by mutableStateOf<NodeStatus?>(null)
    var connectionHealth by mutableStateOf(ConnectionHealth.DISCONNECTED)
    var connectionError by mutableStateOf<String?>(null)
    var lastConnectedAtUnixMs by mutableStateOf<Long?>(null)
    var notice by mutableStateOf<String?>(null)
    var busy by mutableStateOf(false)
    var providers by mutableStateOf(emptyList<Provider>())
    var discovered by mutableStateOf(emptyList<DiscoveryCandidate>())
    var discoveryRunning by mutableStateOf(false)
    var discoveryError by mutableStateOf<String?>(null)
    var discoveryCompleted by mutableStateOf(false)
    var networkPairings by mutableStateOf(emptyList<NetworkPairing>())
    var activeNetworkPairing by mutableStateOf<NetworkPairing?>(null)
    var backups by mutableStateOf(emptyList<RememberedBackup>())
    var transfers by mutableStateOf(emptyList<TransferRecord>())
    var restorePlan by mutableStateOf<RestorePlanPage?>(null)
        private set
    var pairingInvitation by mutableStateOf<JSONObject?>(null)
        private set
    var pairingSession by mutableStateOf<JSONObject?>(null)
        private set
    var pairingConfirmation by mutableStateOf<JSONObject?>(null)
        private set
    var importCandidate by mutableStateOf<JSONObject?>(null)
        private set
    var currentExportedSettings by mutableStateOf<JSONObject?>(null)
        private set

    fun initialize(store: SecureNodeStore) {
        this.store = store
        if (!savedStateHandle.get<Boolean>("initialized").orFalse()) {
            setupName = store.displayName
            setupAddress = store.baseUrl
            setupToken = store.token
            store.enrolledTrust()?.let { trust ->
                setupCaCertificateDer = trust.caCertificateDerBase64.orEmpty()
                setupCaCertificateLabel = if (setupCaCertificateDer.isBlank()) "" else "saved"
                setupCertificatePin = trust.sha256Pin.orEmpty()
            }
            pairingDisplayName = store.displayName.ifBlank { "Android" }
            screen = if (store.baseUrl.isBlank()) Screen.SETUP else Screen.HOME
            savedStateHandle["initialized"] = true
        }
        backups = store.rememberedBackups()
        transfers = store.transfers()
        pairingInvitation = store.workflow(WORKFLOW_INVITATION)
        pairingSession = store.workflow(WORKFLOW_SESSION)
        pairingConfirmation = store.workflow(WORKFLOW_CONFIRMATION)
        restorePlan = store.workflow(WORKFLOW_RESTORE_PLAN)?.let(::restorePlanPageFromPersistence)
        importCandidate = store.workflow(WORKFLOW_IMPORT)
        currentExportedSettings = store.workflow(WORKFLOW_IMPORT_CURRENT)
    }

    fun refreshDurableState() {
        val activeStore = store ?: return
        backups = activeStore.rememberedBackups()
        transfers = activeStore.transfers()
    }

    fun loadProviderDraft(maximumGiB: String, keepFreeGiB: String, lanDiscovery: Boolean) {
        if (providerDraftLoaded) return
        providerMaximumGiB = maximumGiB
        providerKeepFreeGiB = keepFreeGiB
        providerLanDiscovery = lanDiscovery
        providerDraftLoaded = true
    }

    fun persistRestorePlan(value: RestorePlanPage?) {
        restorePlan = value
        store?.saveWorkflow(WORKFLOW_RESTORE_PLAN, value?.toPersistenceJson())
    }

    fun persistPairingInvitation(value: JSONObject?) {
        pairingInvitation = value
        store?.saveWorkflow(WORKFLOW_INVITATION, value)
    }

    fun persistPairingSession(value: JSONObject?) {
        pairingSession = value
        store?.saveWorkflow(WORKFLOW_SESSION, value)
    }

    fun persistPairingConfirmation(value: JSONObject?) {
        pairingConfirmation = value
        store?.saveWorkflow(WORKFLOW_CONFIRMATION, value)
    }

    fun setImportCandidate(value: JSONObject?, current: JSONObject? = currentExportedSettings) {
        importCandidate = value
        currentExportedSettings = current
        store?.saveWorkflow(WORKFLOW_IMPORT, value)
        store?.saveWorkflow(WORKFLOW_IMPORT_CURRENT, if (value == null) null else current)
    }

    fun clearPairing() {
        pairingInput = ""
        pairingError = ""
        persistPairingInvitation(null)
        persistPairingSession(null)
        persistPairingConfirmation(null)
    }

    fun updateNetworkPairing(value: NetworkPairing) {
        networkPairings = (networkPairings.filterNot { it.pairingId == value.pairingId } + value)
            .sortedBy(NetworkPairing::expiresAtUnixMs)
        activeNetworkPairing = value
    }

    fun removeNetworkPairing(pairingId: String) {
        networkPairings = networkPairings.filterNot { it.pairingId == pairingId }
        if (activeNetworkPairing?.pairingId == pairingId) activeNetworkPairing = null
    }

    fun pendingEnrolledTrust(): EnrolledTrust? = when {
        setupCaCertificateDer.isNotBlank() -> EnrolledTrust(caCertificateDerBase64 = setupCaCertificateDer)
        setupCertificatePin.isNotBlank() -> EnrolledTrust(sha256Pin = setupCertificatePin)
        else -> null
    }

    private fun savedString(key: String, default: String): ReadWriteProperty<Any?, String> =
        object : ReadWriteProperty<Any?, String> {
            private var state by mutableStateOf(savedStateHandle[key] ?: default)
            override fun getValue(thisRef: Any?, property: KProperty<*>): String = state
            override fun setValue(thisRef: Any?, property: KProperty<*>, value: String) {
                state = value
                savedStateHandle[key] = value
            }
        }

    private fun savedBoolean(key: String, default: Boolean): ReadWriteProperty<Any?, Boolean> =
        object : ReadWriteProperty<Any?, Boolean> {
            private var state by mutableStateOf(savedStateHandle[key] ?: default)
            override fun getValue(thisRef: Any?, property: KProperty<*>): Boolean = state
            override fun setValue(thisRef: Any?, property: KProperty<*>, value: Boolean) {
                state = value
                savedStateHandle[key] = value
            }
        }

    private fun Boolean?.orFalse(): Boolean = this ?: false

    private companion object {
        const val WORKFLOW_INVITATION = "pairing_invitation"
        const val WORKFLOW_SESSION = "pairing_session"
        const val WORKFLOW_CONFIRMATION = "pairing_confirmation"
        const val WORKFLOW_RESTORE_PLAN = "restore_plan"
        const val WORKFLOW_IMPORT = "settings_import"
        const val WORKFLOW_IMPORT_CURRENT = "settings_import_current"
    }
}
