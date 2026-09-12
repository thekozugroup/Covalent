package life.michaelwong.covalent.sync

import android.content.Context
import java.util.UUID
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

/** Keeps one exact settings request per link until status proves its outcome. */
internal class FolderLinkSettingsChangeStore(
    private val persistence: FolderLinkSettingsChangePersistence,
) {
    constructor(context: Context) : this(AndroidFolderLinkSettingsChangePersistence(context))

    fun records(): List<SavedFolderLinkSettingsChange> = synchronized(LOCK) { recordsLocked() }

    fun prepare(
        folderId: String,
        expectedRevision: Long,
        settings: FolderLinkSettings,
    ): SavedFolderLinkSettingsChange = synchronized(LOCK) {
        val folder = canonicalUuid(folderId)
        require(expectedRevision >= 0)
        val records = recordsLocked()
        records.singleOrNull { it.folderId == folder }?.let { existing ->
            check(!existing.requiresReview && existing.expectedRevision == expectedRevision && existing.settings == settings) {
                "Review the saved settings change before submitting another one."
            }
            return@synchronized existing
        }
        SavedFolderLinkSettingsChange(
            folder,
            UUID.randomUUID().toString(),
            expectedRevision,
            settings,
        ).also { change -> replaceLocked(records + change) }
    }

    fun markForReview(folderId: String, changeId: String) = synchronized(LOCK) {
        val folder = canonicalUuid(folderId)
        val id = canonicalUuid(changeId)
        val records = recordsLocked()
        val existing = records.singleOrNull { it.folderId == folder && it.changeId == id }
            ?: return@synchronized
        if (!existing.requiresReview) {
            replaceLocked(records.map { if (it == existing) it.copy(requiresReview = true) else it })
        }
    }

    fun takeForReview(folderId: String): SavedFolderLinkSettingsChange? = synchronized(LOCK) {
        val folder = canonicalUuid(folderId)
        val records = recordsLocked()
        val existing = records.singleOrNull { it.folderId == folder } ?: return@synchronized null
        replaceLocked(records - existing)
        existing
    }

    fun reconcile(status: FolderSyncStatus) = synchronized(LOCK) {
        val records = recordsLocked()
        val activeFolders = status.shares
            .filter { it.phase != FolderSharePhase.REMOVED && it.linkPolicy != null }
            .mapTo(mutableSetOf()) { it.folderId }
        val updated = records.mapNotNull { saved ->
            if (saved.folderId !in activeFolders) return@mapNotNull null
            val states = status.shares
                .filter { it.folderId == saved.folderId }
                .mapNotNull { it.linkSettings }
                .distinct()
            if (states.isEmpty()) return@mapNotNull saved
            val state = states.singleOrNull()
                ?: throw IllegalStateException("The node returned inconsistent settings for one link.")
            val observed = state.changeId == saved.changeId ||
                state.pendingChange?.changeId == saved.changeId ||
                state.conflictedChange?.changeId == saved.changeId
            if (observed) null
            else if (
                saved.requiresReview || state.revision != saved.expectedRevision ||
                state.pendingChange != null || state.conflictedChange != null
            ) saved.copy(requiresReview = true)
            else saved
        }
        if (updated != records) replaceLocked(updated)
    }

    private fun recordsLocked(): List<SavedFolderLinkSettingsChange> {
        check(persistence.readable) { "Link settings are unavailable until this phone is unlocked." }
        val raw = persistence.read() ?: "[]"
        check(raw.length <= MAX_SERIALIZED_CHARS)
        return runCatching {
            val values = JSONArray(raw)
            check(values.length() <= MAX_RECORDS)
            List(values.length()) { index -> values.getJSONObject(index).toRecord() }.also { records ->
                check(records.map { it.folderId }.toSet().size == records.size)
            }
        }.getOrElse { throw IllegalStateException("Saved link settings are invalid.") }
    }

    private fun replaceLocked(records: List<SavedFolderLinkSettingsChange>) {
        check(records.size <= MAX_RECORDS)
        val serialized = JSONArray().apply { records.forEach { put(it.toJson()) } }.toString()
        check(serialized.length <= MAX_SERIALIZED_CHARS)
        check(persistence.write(serialized)) { "Covalent could not save the link settings change." }
    }

    private companion object {
        val LOCK = Any()
        const val MAX_RECORDS = 128
        const val MAX_SERIALIZED_CHARS = 131_072
    }
}

private fun SavedFolderLinkSettingsChange.toJson() = JSONObject()
    .put("schemaVersion", 1)
    .put("folderId", folderId)
    .put("changeId", changeId)
    .put("expectedRevision", expectedRevision)
    .put("settings", JSONObject()
        .put("deletionPolicy", JSONObject()
            .put("propagateSourceDeletions", settings.deletionPolicy.propagateSourceDeletions)
            .put("restoreLocalDeletions", settings.deletionPolicy.restoreLocalDeletions))
        .put("paused", settings.paused))
    .put("requiresReview", requiresReview)

private fun JSONObject.toRecord(): SavedFolderLinkSettingsChange {
    check(keys().asSequence().toSet() == setOf(
        "schemaVersion", "folderId", "changeId", "expectedRevision", "settings", "requiresReview",
    ))
    check(getInt("schemaVersion") == 1)
    val settings = getJSONObject("settings")
    check(settings.keys().asSequence().toSet() == setOf("deletionPolicy", "paused"))
    val policy = settings.getJSONObject("deletionPolicy")
    check(policy.keys().asSequence().toSet() == setOf("propagateSourceDeletions", "restoreLocalDeletions"))
    return SavedFolderLinkSettingsChange(
        folderId = canonicalUuid(getString("folderId")),
        changeId = canonicalUuid(getString("changeId")).also {
            check(it != "00000000-0000-0000-0000-000000000000")
        },
        expectedRevision = getLong("expectedRevision").also { check(it >= 0) },
        settings = FolderLinkSettings(
            FolderLinkPolicy(
                propagateSourceDeletions = policy.get("propagateSourceDeletions") as? Boolean
                    ?: error("invalid source deletion setting"),
                restoreLocalDeletions = policy.get("restoreLocalDeletions") as? Boolean
                    ?: error("invalid destination deletion setting"),
            ),
            paused = settings.get("paused") as? Boolean ?: error("invalid pause setting"),
        ),
        requiresReview = get("requiresReview") as? Boolean ?: error("invalid review state"),
    )
}

private fun canonicalUuid(value: String): String = UUID.fromString(value).toString().also {
    require(it == value.lowercase())
}
