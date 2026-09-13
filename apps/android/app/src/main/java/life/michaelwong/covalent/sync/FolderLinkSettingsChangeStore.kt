package life.michaelwong.covalent.sync

import android.content.Context
import java.util.UUID
import life.michaelwong.covalent.model.AndroidLinkConditions
import life.michaelwong.covalent.model.FolderLinkCadence
import life.michaelwong.covalent.model.FolderLinkPolicy
import life.michaelwong.covalent.model.FolderLinkSettings
import life.michaelwong.covalent.model.FolderSharePhase
import life.michaelwong.covalent.model.FolderSyncStatus
import life.michaelwong.covalent.node.CredentialProtectedPreferences
import org.json.JSONArray
import org.json.JSONObject

internal data class SavedFolderLinkSettingsChange(
    val folderId: String,
    val changeId: String,
    val expectedRevision: Long,
    val settings: FolderLinkSettings,
    val requiresReview: Boolean = false,
)

internal data class SavedFolderLinkRunRequest(
    val folderId: String,
    val requestId: String,
    val expectedGeneration: Long,
    val settingsRevision: Long,
    val requiresReview: Boolean = false,
)

internal interface FolderLinkSettingsChangePersistence {
    val readable: Boolean
    fun read(): String?
    fun write(value: String): Boolean
}

private class AndroidFolderLinkSettingsChangePersistence(context: Context) :
    FolderLinkSettingsChangePersistence {
    private val preferences = CredentialProtectedPreferences(context, "covalent_folder_link_settings")
    override val readable: Boolean get() = preferences.readable
    override fun read(): String? = preferences.getString("pending_v1", "[]")
    override fun write(value: String): Boolean = preferences.commit { putString("pending_v1", value) }
}

/** Keeps exact settings and run requests until status or a definite response proves their outcome. */
internal class FolderLinkSettingsChangeStore(
    private val persistence: FolderLinkSettingsChangePersistence,
) {
    constructor(context: Context) : this(AndroidFolderLinkSettingsChangePersistence(context))

    fun records(): List<SavedFolderLinkSettingsChange> = synchronized(LOCK) { snapshotLocked().settings }
    fun runRecords(): List<SavedFolderLinkRunRequest> = synchronized(LOCK) { snapshotLocked().runs }

    fun prepare(folderId: String, expectedRevision: Long, settings: FolderLinkSettings) = synchronized(LOCK) {
        val folder = canonicalUuid(folderId)
        require(expectedRevision >= 0)
        val snapshot = snapshotLocked()
        snapshot.settings.singleOrNull { it.folderId == folder }?.let { existing ->
            check(!existing.requiresReview && existing.expectedRevision == expectedRevision && existing.settings == settings) {
                "Review the saved settings change before submitting another one."
            }
            return@synchronized existing
        }
        SavedFolderLinkSettingsChange(folder, UUID.randomUUID().toString(), expectedRevision, settings).also {
            replaceLocked(snapshot.copy(settings = snapshot.settings + it))
        }
    }

    fun prepareRun(folderId: String, expectedGeneration: Long, settingsRevision: Long) = synchronized(LOCK) {
        val folder = canonicalUuid(folderId)
        require(expectedGeneration >= 0 && settingsRevision >= 0)
        val snapshot = snapshotLocked()
        snapshot.runs.singleOrNull { it.folderId == folder }?.let { existing ->
            check(!existing.requiresReview && existing.expectedGeneration == expectedGeneration &&
                existing.settingsRevision == settingsRevision) {
                "Review the saved run request before starting another run."
            }
            return@synchronized existing
        }
        SavedFolderLinkRunRequest(folder, UUID.randomUUID().toString(), expectedGeneration, settingsRevision).also {
            replaceLocked(snapshot.copy(runs = snapshot.runs + it))
        }
    }

    fun markForReview(folderId: String, changeId: String) = synchronized(LOCK) {
        val snapshot = snapshotLocked()
        val folder = canonicalUuid(folderId)
        val id = canonicalUuid(changeId)
        val existing = snapshot.settings.singleOrNull { it.folderId == folder && it.changeId == id }
            ?: return@synchronized
        if (!existing.requiresReview) replaceLocked(snapshot.copy(settings = snapshot.settings.map {
            if (it == existing) it.copy(requiresReview = true) else it
        }))
    }

    fun markRunForReview(folderId: String, requestId: String) = synchronized(LOCK) {
        val snapshot = snapshotLocked()
        val folder = canonicalUuid(folderId)
        val id = canonicalUuid(requestId)
        val existing = snapshot.runs.singleOrNull { it.folderId == folder && it.requestId == id }
            ?: return@synchronized
        if (!existing.requiresReview) replaceLocked(snapshot.copy(runs = snapshot.runs.map {
            if (it == existing) it.copy(requiresReview = true) else it
        }))
    }

    fun takeForReview(folderId: String): SavedFolderLinkSettingsChange? = synchronized(LOCK) {
        val snapshot = snapshotLocked()
        val existing = snapshot.settings.singleOrNull { it.folderId == canonicalUuid(folderId) }
            ?: return@synchronized null
        replaceLocked(snapshot.copy(settings = snapshot.settings - existing))
        existing
    }

    fun takeRunForReview(folderId: String): SavedFolderLinkRunRequest? = synchronized(LOCK) {
        val snapshot = snapshotLocked()
        val existing = snapshot.runs.singleOrNull { it.folderId == canonicalUuid(folderId) }
            ?: return@synchronized null
        replaceLocked(snapshot.copy(runs = snapshot.runs - existing))
        existing
    }

    fun finishRun(folderId: String, requestId: String) = synchronized(LOCK) {
        val snapshot = snapshotLocked()
        val folder = canonicalUuid(folderId)
        val id = canonicalUuid(requestId)
        val existing = snapshot.runs.singleOrNull { it.folderId == folder && it.requestId == id }
            ?: return@synchronized
        replaceLocked(snapshot.copy(runs = snapshot.runs - existing))
    }

    fun reconcile(status: FolderSyncStatus) = synchronized(LOCK) {
        val snapshot = snapshotLocked()
        val activeFolders = status.shares.filter {
            it.phase != FolderSharePhase.REMOVED && it.linkPolicy != null
        }.mapTo(mutableSetOf()) { it.folderId }
        val settings = snapshot.settings.mapNotNull { saved ->
            if (saved.folderId !in activeFolders) return@mapNotNull null
            val states = status.shares.filter { it.folderId == saved.folderId }.mapNotNull { it.linkSettings }.distinct()
            if (states.isEmpty()) return@mapNotNull saved
            val state = states.singleOrNull() ?: error("The node returned inconsistent settings for one link.")
            val observed = state.changeId == saved.changeId || state.pendingChange?.changeId == saved.changeId ||
                state.conflictedChange?.changeId == saved.changeId
            if (observed) null else if (saved.requiresReview || state.revision != saved.expectedRevision ||
                state.pendingChange != null || state.conflictedChange != null) saved.copy(requiresReview = true) else saved
        }
        val runs = snapshot.runs.mapNotNull { saved ->
            if (saved.folderId !in activeFolders) return@mapNotNull null
            val run = status.shares.firstOrNull { it.folderId == saved.folderId }?.linkRun ?: return@mapNotNull saved
            when {
                run.pendingRequest?.requestId == saved.requestId || run.rejectedRequest?.requestId == saved.requestId -> null
                saved.requiresReview || run.generation != saved.expectedGeneration ||
                    run.settingsRevision != saved.settingsRevision || run.pendingRequest != null -> saved.copy(requiresReview = true)
                else -> saved
            }
        }
        val updated = Snapshot(settings, runs)
        if (updated != snapshot) replaceLocked(updated)
    }

    private fun snapshotLocked(): Snapshot {
        check(persistence.readable) { "Link settings are unavailable until this phone is unlocked." }
        val raw = persistence.read() ?: "[]"
        check(raw.length <= MAX_SERIALIZED_CHARS)
        return runCatching {
            if (raw.trimStart().startsWith("[")) Snapshot(JSONArray(raw).toSettings(), emptyList())
            else JSONObject(raw).let { root ->
                check(root.keys().asSequence().toSet() == setOf("schemaVersion", "settings", "runs"))
                check(root.getInt("schemaVersion") == 2)
                Snapshot(root.getJSONArray("settings").toSettings(), root.getJSONArray("runs").toRuns())
            }.also { value ->
                check(value.settings.size <= MAX_RECORDS && value.runs.size <= MAX_RECORDS)
                check(value.settings.map { it.folderId }.distinct().size == value.settings.size)
                check(value.runs.map { it.folderId }.distinct().size == value.runs.size)
            }
        }.getOrElse { throw IllegalStateException("Saved link requests are invalid.") }
    }

    private fun replaceLocked(snapshot: Snapshot) {
        check(snapshot.settings.size <= MAX_RECORDS && snapshot.runs.size <= MAX_RECORDS)
        val serialized = JSONObject().put("schemaVersion", 2)
            .put("settings", JSONArray().apply { snapshot.settings.forEach { put(it.toJson()) } })
            .put("runs", JSONArray().apply { snapshot.runs.forEach { put(it.toJson()) } }).toString()
        check(serialized.length <= MAX_SERIALIZED_CHARS)
        check(persistence.write(serialized)) { "Covalent could not save the link request." }
    }

    private data class Snapshot(
        val settings: List<SavedFolderLinkSettingsChange>,
        val runs: List<SavedFolderLinkRunRequest>,
    )

    private companion object {
        val LOCK = Any()
        const val MAX_RECORDS = 128
        const val MAX_SERIALIZED_CHARS = 131_072
    }
}

private fun JSONArray.toSettings() = List(length()) { getJSONObject(it).toSettingsRecord() }
private fun JSONArray.toRuns() = List(length()) { getJSONObject(it).toRunRecord() }

private fun SavedFolderLinkSettingsChange.toJson() = JSONObject()
    .put("schemaVersion", 2).put("folderId", folderId).put("changeId", changeId)
    .put("expectedRevision", expectedRevision).put("settings", settings.toStoredJson())
    .put("requiresReview", requiresReview)

private fun SavedFolderLinkRunRequest.toJson() = JSONObject()
    .put("folderId", folderId).put("requestId", requestId).put("expectedGeneration", expectedGeneration)
    .put("settingsRevision", settingsRevision).put("requiresReview", requiresReview)

private fun FolderLinkSettings.toStoredJson() = JSONObject()
    .put("deletionPolicy", JSONObject()
        .put("propagateSourceDeletions", deletionPolicy.propagateSourceDeletions)
        .put("restoreLocalDeletions", deletionPolicy.restoreLocalDeletions))
    .put("paused", paused)
    .put("cadence", JSONObject().apply {
        when (val value = this@toStoredJson.cadence) {
            FolderLinkCadence.Manual -> put("mode", "manual")
            FolderLinkCadence.Continuous -> put("mode", "continuous")
            is FolderLinkCadence.Scheduled -> put("mode", "scheduled").put("intervalMinutes", value.intervalMinutes)
        }
    })
    .put("androidConditions", JSONObject()
        .put("wifiOnly", androidConditions.wifiOnly).put("chargingOnly", androidConditions.chargingOnly))

private fun JSONObject.toSettingsRecord(): SavedFolderLinkSettingsChange {
    check(keys().asSequence().toSet() == setOf(
        "schemaVersion", "folderId", "changeId", "expectedRevision", "settings", "requiresReview",
    ))
    val version = getInt("schemaVersion")
    check(version == 1 || version == 2)
    return SavedFolderLinkSettingsChange(
        canonicalUuid(getString("folderId")),
        canonicalUuid(getString("changeId")).also { check(it != ZERO_UUID) },
        getLong("expectedRevision").also { check(it >= 0) },
        getJSONObject("settings").toStoredSettings(version),
        get("requiresReview") as? Boolean ?: error("invalid review state"),
    )
}

private fun JSONObject.toStoredSettings(version: Int): FolderLinkSettings {
    val expected = if (version == 1) setOf("deletionPolicy", "paused")
        else setOf("deletionPolicy", "paused", "cadence", "androidConditions")
    check(keys().asSequence().toSet() == expected)
    val policy = getJSONObject("deletionPolicy")
    check(policy.keys().asSequence().toSet() == setOf("propagateSourceDeletions", "restoreLocalDeletions"))
    val cadence = if (version == 1) FolderLinkCadence.Continuous else getJSONObject("cadence").let {
        when (it.getString("mode")) {
            "manual" -> FolderLinkCadence.Manual.also { _ -> check(it.length() == 1) }
            "continuous" -> FolderLinkCadence.Continuous.also { _ -> check(it.length() == 1) }
            "scheduled" -> FolderLinkCadence.Scheduled(it.getInt("intervalMinutes")).also { _ -> check(it.length() == 2) }
            else -> error("invalid cadence")
        }
    }
    val conditions = if (version == 1) AndroidLinkConditions() else getJSONObject("androidConditions").let {
        check(it.keys().asSequence().toSet() == setOf("wifiOnly", "chargingOnly"))
        AndroidLinkConditions(it.strictBoolean("wifiOnly"), it.strictBoolean("chargingOnly"))
    }
    return FolderLinkSettings(
        FolderLinkPolicy(
            policy.strictBoolean("propagateSourceDeletions"),
            policy.strictBoolean("restoreLocalDeletions"),
        ),
        strictBoolean("paused"), cadence, conditions,
    )
}

private fun JSONObject.toRunRecord(): SavedFolderLinkRunRequest {
    check(keys().asSequence().toSet() == setOf(
        "folderId", "requestId", "expectedGeneration", "settingsRevision", "requiresReview",
    ))
    return SavedFolderLinkRunRequest(
        canonicalUuid(getString("folderId")), canonicalUuid(getString("requestId")).also { check(it != ZERO_UUID) },
        getLong("expectedGeneration").also { check(it >= 0) }, getLong("settingsRevision").also { check(it >= 0) },
        strictBoolean("requiresReview"),
    )
}

private const val ZERO_UUID = "00000000-0000-0000-0000-000000000000"
private fun JSONObject.strictBoolean(name: String): Boolean =
    get(name) as? Boolean ?: error("invalid $name")
private fun canonicalUuid(value: String): String = UUID.fromString(value).toString().also {
    require(it == value.lowercase())
}
