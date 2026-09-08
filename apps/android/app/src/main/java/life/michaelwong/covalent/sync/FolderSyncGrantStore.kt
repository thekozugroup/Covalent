package life.michaelwong.covalent.sync

import android.content.Context
import java.nio.file.Paths
import java.text.Normalizer
import java.util.UUID
import life.michaelwong.covalent.model.FolderSharePhase
import life.michaelwong.covalent.model.FolderSyncStatus
import life.michaelwong.covalent.node.CredentialProtectedPreferences
import org.json.JSONArray
import org.json.JSONObject

internal enum class FolderSyncGrantKind { OFFER, ACCEPTANCE }

/** A private local path capability marker. Its string form never reveals the path. */
internal class FolderSyncGrant(
    val key: String,
    val kind: FolderSyncGrantKind,
    val offerId: String?,
    val folderId: String?,
    val peerId: String?,
    val root: String,
    val label: String?,
) {
    override fun toString(): String = "FolderSyncGrant(kind=$kind, key=<redacted>, root=<redacted>)"
}

internal interface FolderSyncGrantJournal {
    fun records(): List<FolderSyncGrant>
    fun prepareOffer(peerId: String, proposedFolderId: UUID, root: String, label: String): UUID
    fun finishOffer(folderId: UUID, offerId: String)
    fun prepareAcceptance(offerId: String, root: String)
    fun remove(offerId: String)
    fun reconcile(status: FolderSyncStatus)
}

/** Credential-encrypted marker storage written synchronously before a folder API mutation. */
internal class FolderSyncGrantStore(context: Context) : FolderSyncGrantJournal {
    private val preferences = CredentialProtectedPreferences(context, PREFERENCES_NAME)

    override fun records(): List<FolderSyncGrant> = synchronized(JOURNAL_LOCK) { recordsLocked() }

    private fun recordsLocked(): List<FolderSyncGrant> {
        check(preferences.readable) { "Folder sync choices are unavailable until this phone is unlocked." }
        val raw = preferences.getString(KEY_RECORDS, "[]") ?: "[]"
        check(raw.length <= MAX_SERIALIZED_CHARS) { "Saved folder sync choices are invalid." }
        return runCatching {
            val values = JSONArray(raw)
            check(values.length() <= MAX_GRANTS)
            List(values.length()) { index -> values.getJSONObject(index).toGrant() }.also { grants ->
                check(grants.map(FolderSyncGrant::key).toSet().size == grants.size)
            }
        }.getOrElse { throw IllegalStateException("Saved folder sync choices are invalid.") }
    }

    override fun prepareOffer(peerId: String, proposedFolderId: UUID, root: String, label: String): UUID =
        synchronized(JOURNAL_LOCK) {
            require(label.isNotBlank() && label.length <= 256 && label.none(Char::isISOControl))
            val canonicalPeer = canonicalUuid(peerId)
            val canonicalRoot = validateStoredRoot(root)
            val grants = recordsLocked()
            val folderId = grants
                .singleOrNull {
                    it.kind == FolderSyncGrantKind.OFFER && it.offerId == null &&
                        it.peerId == canonicalPeer && it.root == canonicalRoot && it.label == label
                }
                ?.folderId
                ?.let(UUID::fromString)
                ?: proposedFolderId
            val key = "folder:$folderId"
            replaceLocked(grants.filterNot { it.key == key } + FolderSyncGrant(
                key = key,
                kind = FolderSyncGrantKind.OFFER,
                offerId = null,
                folderId = folderId.toString(),
                peerId = canonicalPeer,
                root = canonicalRoot,
                label = label,
            ))
            folderId
        }

    override fun finishOffer(folderId: UUID, offerId: String) = synchronized(JOURNAL_LOCK) {
        val id = canonicalUuid(offerId)
        val key = "folder:$folderId"
        val grants = recordsLocked()
        val pending = grants.singleOrNull { it.key == key && it.kind == FolderSyncGrantKind.OFFER }
            ?: throw IllegalStateException("The durable folder choice is missing.")
        replaceLocked(grants.filterNot { it.key == key || it.offerId == id } + FolderSyncGrant(
            key = "offer:$id",
            kind = pending.kind,
            offerId = id,
            folderId = pending.folderId,
            peerId = pending.peerId,
            root = pending.root,
            label = pending.label,
        ))
    }

    override fun prepareAcceptance(offerId: String, root: String) = synchronized(JOURNAL_LOCK) {
        val id = canonicalUuid(offerId)
        val key = "offer:$id"
        replaceLocked(recordsLocked().filterNot { it.key == key } + FolderSyncGrant(
            key = key,
            kind = FolderSyncGrantKind.ACCEPTANCE,
            offerId = id,
            folderId = null,
            peerId = null,
            root = validateStoredRoot(root),
            label = null,
        ))
    }

    override fun remove(offerId: String) = synchronized(JOURNAL_LOCK) {
        val id = canonicalUuid(offerId)
        replaceLocked(recordsLocked().filterNot { it.offerId == id })
    }

    override fun reconcile(status: FolderSyncStatus) = synchronized(JOURNAL_LOCK) {
        val removed = status.shares.filter { it.phase == FolderSharePhase.REMOVED }.map { it.offerId }.toSet()
        val original = recordsLocked()
        var grants = original.filterNot { it.offerId in removed }
        val pending = grants.filter { it.kind == FolderSyncGrantKind.OFFER && it.offerId == null }
        pending.forEach { grant ->
            val share = status.shares.singleOrNull { !it.incoming && it.folderId == grant.folderId }
                ?: return@forEach
            grants = grants.filterNot { it.key == grant.key } + FolderSyncGrant(
                key = "offer:${share.offerId}",
                kind = grant.kind,
                offerId = share.offerId,
                folderId = grant.folderId,
                peerId = grant.peerId,
                root = grant.root,
                label = grant.label,
            )
        }
        if (grants.map(FolderSyncGrant::key) != original.map(FolderSyncGrant::key)) replaceLocked(grants)
    }

    private fun replaceLocked(grants: List<FolderSyncGrant>) {
        check(grants.size <= MAX_GRANTS) { "This phone has too many folder sync choices." }
        val encoded = JSONArray().apply { grants.sortedBy(FolderSyncGrant::key).forEach { put(it.toJson()) } }.toString()
        check(encoded.length <= MAX_SERIALIZED_CHARS) { "Saved folder sync choices are too large." }
        check(preferences.commit { putString(KEY_RECORDS, encoded) }) {
            "Android could not durably save the folder choice."
        }
    }

    private fun JSONObject.toGrant(): FolderSyncGrant {
        val expected = setOf("schemaVersion", "key", "kind", "offerId", "folderId", "peerId", "root", "label")
        check(keys().asSequence().toSet() == expected)
        check(getInt("schemaVersion") == 1)
        val kind = when (getString("kind")) {
            "offer" -> FolderSyncGrantKind.OFFER
            "acceptance" -> FolderSyncGrantKind.ACCEPTANCE
            else -> error("invalid")
        }
        val offerId = optionalString("offerId")?.let(::canonicalUuid)
        val folderId = optionalString("folderId")?.let(::canonicalUuid)
        val peerId = optionalString("peerId")?.let(::canonicalUuid)
        val root = validateStoredRoot(getString("root"))
        val label = optionalString("label")?.also {
            check(it.isNotBlank() && it.length <= 256 && it.none(Char::isISOControl))
        }
        val key = getString("key")
        check(key == offerId?.let { "offer:$it" } || key == folderId?.let { "folder:$it" })
        when (kind) {
            FolderSyncGrantKind.OFFER -> {
                check(folderId != null && peerId != null && label != null)
                check(
                    (offerId == null && key == "folder:$folderId") ||
                        (offerId != null && key == "offer:$offerId"),
                )
            }
            FolderSyncGrantKind.ACCEPTANCE -> {
                check(offerId != null && folderId == null && peerId == null && label == null)
            }
        }
        return FolderSyncGrant(key, kind, offerId, folderId, peerId, root, label)
    }

    private fun FolderSyncGrant.toJson(): JSONObject = JSONObject()
        .put("schemaVersion", 1)
        .put("key", key)
        .put("kind", kind.name.lowercase())
        .put("offerId", offerId ?: JSONObject.NULL)
        .put("folderId", folderId ?: JSONObject.NULL)
        .put("peerId", peerId ?: JSONObject.NULL)
        .put("root", root)
        .put("label", label ?: JSONObject.NULL)

    private fun JSONObject.optionalString(key: String): String? =
        if (!has(key) || isNull(key)) null else getString(key)

    companion object {
        private const val PREFERENCES_NAME = "covalent_folder_sync_grants"
        private const val KEY_RECORDS = "records_v1"
        private const val MAX_GRANTS = 128
        private const val MAX_SERIALIZED_CHARS = 1_048_576
        private val JOURNAL_LOCK = Any()
    }
}

private fun validateStoredRoot(value: String): String {
    check(
        value.length in 1..4_096 && value.startsWith('/') && value.none(Char::isISOControl) &&
            Normalizer.isNormalized(value, Normalizer.Form.NFC) && Paths.get(value).normalize().toString() == value,
    ) {
        "The folder choice is invalid."
    }
    return value
}

private fun canonicalUuid(value: String): String = runCatching { UUID.fromString(value).toString() }
    .getOrElse { throw IllegalArgumentException("The folder identifier is invalid.") }
    .also { require(it == value) { "The folder identifier is invalid." } }
