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
    val pendingRoot: String? = null,
    val pendingRemoval: Boolean = false,
) {
    override fun toString(): String = "FolderSyncGrant(kind=$kind, key=<redacted>, root=<redacted>)"
}

internal interface FolderSyncGrantJournal {
    fun records(): List<FolderSyncGrant>
    fun prepareOffer(peerId: String, proposedFolderId: UUID, root: String, label: String): UUID
    fun finishOffer(folderId: UUID, offerId: String)
    fun prepareAcceptance(offerId: String, root: String)
    fun prepareRepair(offerId: String, root: String)
    fun finishRepair(offerId: String, root: String)
    fun pendingRepairRoot(offerId: String): String?
    fun prepareRemoval(offerId: String)
    fun finishRemoval(offerId: String)
    fun reconcile(status: FolderSyncStatus)
}

internal interface FolderSyncGrantPersistence {
    val readable: Boolean
    fun read(): String?
    fun write(value: String): Boolean
}

private class AndroidFolderSyncGrantPersistence(context: Context) : FolderSyncGrantPersistence {
    private val preferences = CredentialProtectedPreferences(context, "covalent_folder_sync_grants")
    override val readable: Boolean get() = preferences.readable
    override fun read(): String? = preferences.getString("records_v1", "[]")
    override fun write(value: String): Boolean = preferences.commit { putString("records_v1", value) }
}

/** Credential-encrypted marker storage written synchronously before a folder API mutation. */
internal class FolderSyncGrantStore(
    private val persistence: FolderSyncGrantPersistence,
) : FolderSyncGrantJournal {
    constructor(context: Context) : this(AndroidFolderSyncGrantPersistence(context))

    override fun records(): List<FolderSyncGrant> = synchronized(JOURNAL_LOCK) { recordsLocked() }

    /** A prepared capability change keeps the folder worker stopped until exact acknowledgement. */
    internal fun hasPendingCapabilityChange(): Boolean = synchronized(JOURNAL_LOCK) {
        recordsLocked().any { it.pendingRoot != null || it.pendingRemoval }
    }

    private fun recordsLocked(): List<FolderSyncGrant> {
        check(persistence.readable) { "Folder sync choices are unavailable until this phone is unlocked." }
        val raw = persistence.read() ?: "[]"
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
            pendingRoot = null,
            pendingRemoval = false,
        ))
    }

    override fun prepareAcceptance(offerId: String, root: String) = synchronized(JOURNAL_LOCK) {
        val id = canonicalUuid(offerId)
        val key = "offer:$id"
        val selectedRoot = validateStoredRoot(root)
        val grants = recordsLocked()
        grants.singleOrNull { it.offerId == id }?.let { existing ->
            check(existing.kind == FolderSyncGrantKind.ACCEPTANCE && existing.root == selectedRoot) {
                "This invitation already has a saved folder choice. Use folder repair to change it."
            }
            check(existing.pendingRoot == null && !existing.pendingRemoval) {
                "Finish the saved folder change before accepting this invitation again."
            }
            return@synchronized
        }
        replaceLocked(grants + FolderSyncGrant(
            key = key,
            kind = FolderSyncGrantKind.ACCEPTANCE,
            offerId = id,
            folderId = null,
            peerId = null,
            root = selectedRoot,
            label = null,
            pendingRoot = null,
            pendingRemoval = false,
        ))
    }

    override fun prepareRepair(offerId: String, root: String) = synchronized(JOURNAL_LOCK) {
        val id = canonicalUuid(offerId)
        val selectedRoot = validateStoredRoot(root)
        val grants = recordsLocked()
        val existing = grants.singleOrNull { it.offerId == id }
            ?: throw IllegalStateException("The durable folder choice is missing.")
        check(!existing.pendingRemoval) { "This folder is already being removed." }
        existing.pendingRoot?.let {
            check(it == selectedRoot) { "Finish the saved folder repair before choosing another folder." }
            return@synchronized
        }
        replaceLocked(grants.replace(existing, existing.withPendingRoot(selectedRoot)))
    }

    override fun finishRepair(offerId: String, root: String) = synchronized(JOURNAL_LOCK) {
        val id = canonicalUuid(offerId)
        val selectedRoot = validateStoredRoot(root)
        val grants = recordsLocked()
        val existing = grants.singleOrNull { it.offerId == id }
            ?: throw IllegalStateException("The durable folder choice is missing.")
        check(!existing.pendingRemoval && existing.pendingRoot == selectedRoot) {
            "The acknowledged folder repair does not match the durable choice."
        }
        replaceLocked(grants.replace(existing, existing.withAcknowledgedRoot(selectedRoot)))
    }

    override fun pendingRepairRoot(offerId: String): String? = synchronized(JOURNAL_LOCK) {
        val id = canonicalUuid(offerId)
        recordsLocked().singleOrNull { it.offerId == id && !it.pendingRemoval }?.pendingRoot
    }

    override fun prepareRemoval(offerId: String) = synchronized(JOURNAL_LOCK) {
        val id = canonicalUuid(offerId)
        val grants = recordsLocked()
        val existing = grants.singleOrNull { it.offerId == id }
            // An unaccepted incoming invitation has no local folder capability.
            // Its removal is still durably recorded by the authenticated node.
            // This also permits an exact retry after local acknowledgement.
            ?: return@synchronized
        if (existing.pendingRemoval) return@synchronized
        replaceLocked(grants.replace(existing, existing.asPendingRemoval()))
    }

    override fun finishRemoval(offerId: String) = synchronized(JOURNAL_LOCK) {
        val id = canonicalUuid(offerId)
        val grants = recordsLocked()
        val existing = grants.singleOrNull { it.offerId == id }
            ?: return@synchronized
        check(existing.pendingRemoval) { "The folder removal was not durably prepared." }
        replaceLocked(grants.filterNot { it.offerId == id })
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
                pendingRoot = grant.pendingRoot,
                pendingRemoval = grant.pendingRemoval,
            )
        }
        if (grants.map(FolderSyncGrant::key) != original.map(FolderSyncGrant::key)) replaceLocked(grants)
    }

    private fun replaceLocked(grants: List<FolderSyncGrant>) {
        check(grants.size <= MAX_GRANTS) { "This phone has too many folder sync choices." }
        val encoded = JSONArray().apply { grants.sortedBy(FolderSyncGrant::key).forEach { put(it.toJson()) } }.toString()
        check(encoded.length <= MAX_SERIALIZED_CHARS) { "Saved folder sync choices are too large." }
        check(persistence.write(encoded)) {
            "Android could not durably save the folder choice."
        }
    }

    private fun JSONObject.toGrant(): FolderSyncGrant {
        val schema = getInt("schemaVersion")
        val expectedV1 = setOf("schemaVersion", "key", "kind", "offerId", "folderId", "peerId", "root", "label")
        val expectedV2 = expectedV1 + setOf("pendingRoot", "pendingRemoval")
        check(keys().asSequence().toSet() == if (schema == 1) expectedV1 else expectedV2)
        check(schema in 1..2)
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
        val pendingRoot = if (schema == 2) optionalString("pendingRoot")?.let(::validateStoredRoot) else null
        val pendingRemoval = schema == 2 && getBoolean("pendingRemoval")
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
        check(offerId != null || (pendingRoot == null && !pendingRemoval))
        check(pendingRoot == null || !pendingRemoval)
        return FolderSyncGrant(
            key,
            kind,
            offerId,
            folderId,
            peerId,
            root,
            label,
            pendingRoot,
            pendingRemoval,
        )
    }

    private fun FolderSyncGrant.toJson(): JSONObject = JSONObject()
        .put("schemaVersion", 2)
        .put("key", key)
        .put("kind", kind.name.lowercase())
        .put("offerId", offerId ?: JSONObject.NULL)
        .put("folderId", folderId ?: JSONObject.NULL)
        .put("peerId", peerId ?: JSONObject.NULL)
        .put("root", root)
        .put("label", label ?: JSONObject.NULL)
        .put("pendingRoot", pendingRoot ?: JSONObject.NULL)
        .put("pendingRemoval", pendingRemoval)

    private fun List<FolderSyncGrant>.replace(
        existing: FolderSyncGrant,
        replacement: FolderSyncGrant,
    ): List<FolderSyncGrant> = map { if (it.key == existing.key) replacement else it }

    private fun FolderSyncGrant.withPendingRoot(value: String) = FolderSyncGrant(
        key, kind, offerId, folderId, peerId, root, label, value, false,
    )

    private fun FolderSyncGrant.withAcknowledgedRoot(value: String) = FolderSyncGrant(
        key, kind, offerId, folderId, peerId, value, label, null, false,
    )

    private fun FolderSyncGrant.asPendingRemoval() = FolderSyncGrant(
        key, kind, offerId, folderId, peerId, root, label, null, true,
    )

    private fun JSONObject.optionalString(key: String): String? =
        if (!has(key) || isNull(key)) null else getString(key)

    companion object {
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
