package life.michaelwong.covalent.appliance

import android.content.Context
import java.util.Base64
import java.util.UUID
import life.michaelwong.covalent.data.NodeApiException
import life.michaelwong.covalent.node.CredentialProtectedPreferences
import org.json.JSONObject

/** Full paired key is compared; the short fingerprint is display-only. */
internal data class ApplianceIdentity(val deviceId: String, val publicKey: String, val fingerprint: String) {
    init {
        requireCanonicalId(deviceId)
        val decoded = Base64.getUrlDecoder().decode(publicKey)
        require(decoded.size == 32 && Base64.getUrlEncoder().withoutPadding().encodeToString(decoded) == publicKey)
        require(fingerprint.matches(Regex("[0-9a-f]{20}")))
    }
    override fun toString() = "ApplianceIdentity(<redacted>)"
}

internal data class ApplianceContext(val local: ApplianceIdentity, val paired: List<ApplianceIdentity>)

internal enum class ApplianceSyncState(val wire: String) {
    UNAVAILABLE("unavailable"), STOPPED("stopped"), SCANNING("initialScanning"),
    RUNNING("running"), STOPPING("stillStopping"), NEEDS_ATTENTION("needsAttention"),
}

internal data class ApplianceObservation(
    val bondGeneration: String,
    val syncState: ApplianceSyncState,
    val shareRecords: Long?,
    val healthFresh: Boolean,
)

internal data class SelectedAppliance(
    val local: ApplianceIdentity,
    val appliance: ApplianceIdentity,
    val name: String,
    val confirmedGeneration: String? = null,
) {
    init {
        require(local.deviceId != appliance.deviceId)
        require(name.isNotBlank() && name.length <= 256 && name.none(Char::isISOControl))
        confirmedGeneration?.let(::requireCanonicalId)
    }
    override fun toString() = "SelectedAppliance(<redacted>)"
}

internal interface ApplianceSelectionPersistence {
    val readable: Boolean
    fun read(): String?
    fun write(value: String): Boolean
}

internal class ApplianceSelectionStore(private val disk: ApplianceSelectionPersistence) {
    constructor(context: Context) : this(object : ApplianceSelectionPersistence {
        private val preferences = CredentialProtectedPreferences(context, "interlink_selected_appliance")
        override val readable get() = preferences.readable
        override fun read() = preferences.getString("selection_v1", null)
        override fun write(value: String) = preferences.commit { putString("selection_v1", value) }
    })

    fun read(): SelectedAppliance? = synchronized(LOCK) { readLocked() }

    /** A stale response may never replace a newer owner choice. */
    fun replace(expected: SelectedAppliance?, next: SelectedAppliance) = synchronized(LOCK) {
        check(readLocked() == expected) { "The selected Pi changed. Refresh before trying again." }
        val value = JSONObject().put("schemaVersion", 1)
            .put("local", next.local.toJson()).put("appliance", next.appliance.toJson())
            .put("name", next.name).put("confirmedGeneration", next.confirmedGeneration ?: JSONObject.NULL)
        check(disk.write(value.toString())) { "The Pi choice could not be saved. Your previous choice is retained." }
    }

    private fun readLocked(): SelectedAppliance? {
        check(disk.readable) { "Unlock this phone to read its saved Pi." }
        val raw = disk.read() ?: return null
        check(raw.length <= 8192) { "The saved Pi needs attention." }
        val value = JSONObject(raw)
        value.exactKeys("schemaVersion", "local", "appliance", "name", "confirmedGeneration")
        check(value.strictLong("schemaVersion") == 1L)
        return SelectedAppliance(value.getJSONObject("local").identity(), value.getJSONObject("appliance").identity(),
            value.strictString("name"), if (value.isNull("confirmedGeneration")) null else value.strictString("confirmedGeneration"))
    }

    private companion object { val LOCK = Any() }
}

internal enum class ApplianceConnectionState {
    NO_SELECTION, CHECKING, CONNECTED, APPROVAL_REQUIRED, UNAVAILABLE,
    PHONE_UNAVAILABLE, IDENTITY_CHANGED, UNSUPPORTED, SAVED_CHOICE_UNAVAILABLE,
}

internal data class ApplianceCandidate(val identity: ApplianceIdentity, val name: String)
internal data class ApplianceDashboardState(
    val selection: SelectedAppliance? = null,
    val connection: ApplianceConnectionState = ApplianceConnectionState.CHECKING,
    val context: ApplianceContext? = null,
    val candidates: List<ApplianceCandidate> = emptyList(),
    val observation: ApplianceObservation? = null,
)

internal interface ApplianceDashboardApi {
    fun context(): ApplianceContext
    fun names(): Map<String, String>
    fun status(peerId: String, localId: String): ApplianceObservation
}

/** Read-only remote operations; selection never grants or revokes Pi authority. */
internal class ApplianceDashboardController(
    private val store: ApplianceSelectionStore,
    private val api: ApplianceDashboardApi,
) {
    fun refresh(phoneReady: Boolean = true): ApplianceDashboardState {
        val selected = try { store.read() } catch (_: Exception) {
            return ApplianceDashboardState(connection = ApplianceConnectionState.SAVED_CHOICE_UNAVAILABLE)
        }
        if (!phoneReady) return ApplianceDashboardState(selected,
            if (selected == null) ApplianceConnectionState.NO_SELECTION else ApplianceConnectionState.PHONE_UNAVAILABLE)
        val context = try { api.context() } catch (error: Exception) {
            return ApplianceDashboardState(selected, if (error.isUnsupported()) ApplianceConnectionState.UNSUPPORTED
                else ApplianceConnectionState.PHONE_UNAVAILABLE)
        }
        val names = runCatching(api::names).getOrDefault(emptyMap())
        val candidates = context.paired.map { peer ->
            val name = names[peer.deviceId]?.takeIf { it.isNotBlank() && it.length <= 256 && it.none(Char::isISOControl) }
                ?: "Paired device"
            ApplianceCandidate(peer, name)
        }
        val state = ApplianceDashboardState(selected, ApplianceConnectionState.NO_SELECTION, context, candidates)
        if (selected == null) return state
        if (context.local != selected.local || context.paired.none { it == selected.appliance }) {
            return state.copy(connection = ApplianceConnectionState.IDENTITY_CHANGED)
        }
        val observation = try { api.status(selected.appliance.deviceId, selected.local.deviceId) } catch (error: Exception) {
            val failure = when {
                error is NodeApiException && error.hasExplicitRetryContract &&
                    error.protocolVersion == 1 && error.statusCode == 403 &&
                    error.code == "controller_rejected" -> ApplianceConnectionState.APPROVAL_REQUIRED
                error.isUnsupported() -> ApplianceConnectionState.UNSUPPORTED
                else -> ApplianceConnectionState.UNAVAILABLE
            }
            return state.copy(connection = failure)
        }
        val confirmed = selected.copy(confirmedGeneration = observation.bondGeneration)
        try {
            if (confirmed != selected) store.replace(selected, confirmed)
            // Even a status read with an unchanged generation must reject a newer selection.
            check(store.read() == confirmed)
        } catch (_: Exception) {
            return state.copy(connection = ApplianceConnectionState.SAVED_CHOICE_UNAVAILABLE)
        }
        return state.copy(selection = confirmed, connection = ApplianceConnectionState.CONNECTED, observation = observation)
    }

    fun select(expected: SelectedAppliance?, shownContext: ApplianceContext, candidate: ApplianceCandidate) {
        val current = api.context()
        check(current.local == shownContext.local && current.paired.any { it == candidate.identity }) {
            "The pairing changed. Refresh before choosing this Pi."
        }
        store.replace(expected, SelectedAppliance(current.local, candidate.identity, candidate.name))
    }
}

private fun Exception.isUnsupported() = this is NodeApiException && statusCode in setOf(404, 405, 501)

internal fun parseApplianceContext(json: JSONObject): ApplianceContext {
    json.exactKeys("bond", "pairedDevices")
    val bond = json.getJSONObject("bond")
    bond.exactKeys("schemaVersion", "generation", "appliance", "controller")
    require(bond.strictLong("schemaVersion") == 1L)
    requireCanonicalId(bond.strictString("generation"))
    val local = bond.getJSONObject("appliance").identity()
    if (!bond.isNull("controller")) require(bond.getJSONObject("controller").identity().deviceId != local.deviceId)
    val peers = json.getJSONArray("pairedDevices")
    require(peers.length() <= 128)
    val paired = List(peers.length()) { peers.getJSONObject(it).identity() }
    require(paired.map { it.deviceId }.toSet().size == paired.size && paired.none { it.deviceId == local.deviceId })
    return ApplianceContext(local, paired)
}

internal fun parseApplianceObservation(json: JSONObject, peerId: String, localId: String): ApplianceObservation {
    json.exactKeys("schemaVersion", "applianceId", "controllerId", "bondGeneration", "syncAvailable", "syncLifecycle",
        "shareRecords", "folderHealthFresh", "cloudBackupVerificationAvailable", "sourceCleanupAvailable")
    require(json.strictLong("schemaVersion") == 1L)
    requireCanonicalId(peerId); requireCanonicalId(localId)
    require(json.strictString("applianceId") == peerId && json.strictString("controllerId") == localId)
    val generation = json.strictString("bondGeneration").also(::requireCanonicalId)
    val state = ApplianceSyncState.entries.single { it.wire == json.strictString("syncLifecycle") }
    val count = if (json.isNull("shareRecords")) null else json.strictLong("shareRecords").also { require(it >= 0) }
    val fresh = json.strictBoolean("folderHealthFresh")
    require(!json.strictBoolean("cloudBackupVerificationAvailable") && !json.strictBoolean("sourceCleanupAvailable"))
    if (json.strictBoolean("syncAvailable")) require(state != ApplianceSyncState.UNAVAILABLE && count != null)
    else require(state == ApplianceSyncState.UNAVAILABLE && count == null && !fresh)
    return ApplianceObservation(generation, state, count, fresh)
}

private fun requireCanonicalId(value: String) {
    val id = UUID.fromString(value)
    require(id.toString() == value && id != UUID(0, 0))
}
private fun JSONObject.identity(): ApplianceIdentity {
    exactKeys("deviceId", "publicKey", "fingerprint")
    return ApplianceIdentity(strictString("deviceId"), strictString("publicKey"), strictString("fingerprint"))
}
private fun ApplianceIdentity.toJson() = JSONObject().put("deviceId", deviceId).put("publicKey", publicKey).put("fingerprint", fingerprint)
private fun JSONObject.exactKeys(vararg keys: String) { require(keys().asSequence().toSet() == keys.toSet()) }
private fun JSONObject.strictString(key: String) = (get(key) as? String) ?: error("Invalid control response.")
private fun JSONObject.strictBoolean(key: String) = (get(key) as? Boolean) ?: error("Invalid control response.")
private fun JSONObject.strictLong(key: String): Long = when (val value = get(key)) {
    is Int -> value.toLong()
    is Long -> value
    else -> error("Invalid control response.")
}
