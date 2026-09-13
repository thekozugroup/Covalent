package life.michaelwong.covalent.sync

import android.content.ContentResolver
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.provider.DocumentsContract
import androidx.core.net.toUri
import java.util.UUID
import life.michaelwong.covalent.node.CredentialProtectedPreferences
import org.json.JSONArray
import org.json.JSONObject

internal data class SafFolderGrant(val id: UUID, val treeUri: Uri) {
    val selectedRoot: String get() = "$TOKEN_PREFIX$id"

    override fun toString(): String = "SafFolderGrant(id=$id, treeUri=<redacted>)"

    companion object {
        const val TOKEN_PREFIX = "covalent-saf:"

        fun idFromSelectedRoot(value: String): UUID {
            require(value.startsWith(TOKEN_PREFIX)) { "The folder choice is invalid." }
            val raw = value.removePrefix(TOKEN_PREFIX)
            val id = runCatching { UUID.fromString(raw) }
                .getOrElse { throw IllegalArgumentException("The folder choice is invalid.") }
            require(raw == id.toString()) { "The folder choice is invalid." }
            return id
        }
    }
}

internal interface SafFolderGrantPersistence {
    val readable: Boolean
    fun read(): String?
    fun write(value: String): Boolean
}

private class AndroidSafFolderGrantPersistence(context: Context) : SafFolderGrantPersistence {
    private val preferences = CredentialProtectedPreferences(context, "covalent_saf_folder_grants")
    override val readable: Boolean get() = preferences.readable
    override fun read(): String? = preferences.getString("records_v1", "[]")
    override fun write(value: String): Boolean = preferences.commit { putString("records_v1", value) }
}

/** Keeps the SAF URI private on Android. The shared journal receives only `covalent-saf:<UUID>`. */
internal class SafFolderGrantStore(
    private val persistence: SafFolderGrantPersistence,
    private val persistedPermissions: () -> List<Pair<Uri, Boolean>>,
    private val takePermission: (Uri) -> Unit,
) {
    constructor(context: Context) : this(
        AndroidSafFolderGrantPersistence(context.applicationContext),
        {
            context.contentResolver.persistedUriPermissions.map {
                it.uri to (it.isReadPermission && it.isWritePermission)
            }
        },
        { uri ->
            context.contentResolver.takePersistableUriPermission(
                uri,
                Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION,
            )
        },
    )

    fun records(): List<SafFolderGrant> = synchronized(LOCK) { recordsLocked() }

    fun persist(treeUri: Uri): SafFolderGrant = synchronized(LOCK) {
        requireTreeUri(treeUri)
        takePermission(treeUri)
        check(hasPermission(treeUri)) { "Android did not retain access to the selected folder." }
        val records = recordsLocked()
        records.singleOrNull { it.treeUri == treeUri }?.let { return@synchronized it }
        check(records.size < MAX_GRANTS) { "This phone has too many saved folder choices." }
        val grant = SafFolderGrant(UUID.randomUUID(), treeUri)
        replaceLocked(records + grant)
        grant
    }

    fun requireSelectedRoot(value: String): String {
        val id = SafFolderGrant.idFromSelectedRoot(value)
        val grant = synchronized(LOCK) { recordsLocked().singleOrNull { it.id == id } }
            ?: throw IllegalArgumentException("The folder choice is unavailable.")
        check(hasPermission(grant.treeUri)) { "Android no longer allows access to the selected folder." }
        return grant.selectedRoot
    }

    fun allAccessible(): List<SafFolderGrant> = synchronized(LOCK) {
        recordsLocked().filter { hasPermission(it.treeUri) }
    }

    fun allRecordsAccessible(): Boolean = synchronized(LOCK) {
        recordsLocked().all { hasPermission(it.treeUri) }
    }

    private fun recordsLocked(): List<SafFolderGrant> {
        check(persistence.readable) { "Folder choices are unavailable until this phone is unlocked." }
        val encoded = persistence.read() ?: "[]"
        check(encoded.length <= MAX_SERIALIZED_CHARS)
        return runCatching {
            val values = JSONArray(encoded)
            check(values.length() <= MAX_GRANTS)
            List(values.length()) { index ->
                val value = values.getJSONObject(index)
                check(value.keys().asSequence().toSet() == setOf("schemaVersion", "grantId", "treeUri"))
                check(value.getInt("schemaVersion") == 1)
                val id = UUID.fromString(value.getString("grantId"))
                check(value.getString("grantId") == id.toString())
                val uri = value.getString("treeUri").toUri()
                requireTreeUri(uri)
                SafFolderGrant(id, uri)
            }.also { records ->
                check(records.map { it.id }.toSet().size == records.size)
                check(records.map { it.treeUri }.toSet().size == records.size)
            }
        }.getOrElse { throw IllegalStateException("Saved folder choices are invalid.") }
    }

    private fun replaceLocked(records: List<SafFolderGrant>) {
        val encoded = JSONArray().apply {
            records.sortedBy { it.id.toString() }.forEach { grant ->
                put(JSONObject()
                    .put("schemaVersion", 1)
                    .put("grantId", grant.id.toString())
                    .put("treeUri", grant.treeUri.toString()))
            }
        }.toString()
        check(encoded.length <= MAX_SERIALIZED_CHARS)
        check(persistence.write(encoded)) { "Android could not save the folder choice." }
    }

    private fun hasPermission(uri: Uri): Boolean = persistedPermissions().any { it.first == uri && it.second }

    companion object {
        private val LOCK = Any()
        private const val MAX_GRANTS = 128
        private const val MAX_SERIALIZED_CHARS = 1_048_576

        internal fun requireTreeUri(uri: Uri) {
            require(uri.scheme == ContentResolver.SCHEME_CONTENT && DocumentsContract.isTreeUri(uri)) {
                "Choose a folder through Android's folder picker."
            }
            val treeId = runCatching { DocumentsContract.getTreeDocumentId(uri) }.getOrNull()
            val canonical = treeId?.let { DocumentsContract.buildTreeDocumentUri(uri.authority, it) }
            require(!treeId.isNullOrEmpty() && uri == canonical && uri.toString().length <= 16_384) {
                "The selected folder is invalid."
            }
        }
    }
}
