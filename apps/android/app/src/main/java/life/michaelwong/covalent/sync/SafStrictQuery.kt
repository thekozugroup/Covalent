package life.michaelwong.covalent.sync

import android.content.ContentResolver
import android.database.Cursor
import android.net.Uri
import android.os.CancellationSignal
import android.os.OperationCanceledException
import android.provider.DocumentsContract
import java.nio.charset.CharacterCodingException
import java.nio.charset.CodingErrorAction
import java.nio.charset.StandardCharsets
import java.text.Normalizer
import java.util.Locale

/**
 * A fail-closed, metadata-only observation of a SAF tree.
 *
 * This is not a content proof, an adoption decision, a deletion signal, or a successful sync.
 * Native SyncPath validation and content hashing remain mandatory before any signed operation.
 * Two matching scans and cursor-extra sampling detect observed changes; SAF exposes no atomic
 * provider snapshot, so later authorization must still revalidate against durable sync state.
 */
internal class SafSyncMetadataObservation internal constructor(
    val rootDocumentId: String,
    entries: List<SafSyncMetadataEntry>,
) {
    val entries: List<SafSyncMetadataEntry> = entries.toList()

    override fun toString(): String =
        "SafSyncMetadataObservation(entries=${entries.size}, contentVerified=false)"
}

internal enum class SafSyncMetadataKind {
    FILE,
    DIRECTORY,
}

internal class SafSyncMetadataEntry internal constructor(
    val documentId: String,
    val parentDocumentId: String,
    val displayName: String,
    val kind: SafSyncMetadataKind,
    val mimeType: String,
    val flags: Int,
    val sizeBytes: Long?,
    val lastModifiedMillis: Long?,
    val depth: Int,
) {
    override fun toString(): String =
        "SafSyncMetadataEntry(kind=$kind, depth=$depth, nameRedacted=true)"

    internal fun sameMetadata(other: SafSyncMetadataEntry): Boolean =
        documentId == other.documentId &&
            parentDocumentId == other.parentDocumentId &&
            displayName == other.displayName &&
            kind == other.kind &&
            mimeType == other.mimeType &&
            flags == other.flags &&
            sizeBytes == other.sizeBytes &&
            lastModifiedMillis == other.lastModifiedMillis &&
            depth == other.depth
}

internal data class SafSyncInventoryLimits(
    val maxEntries: Int = 25_000,
    val maxDirectories: Int = 5_000,
    val maxDepth: Int = 64,
    val maxNameUtf8Bytes: Int = 1_024,
    val maxDocumentIdUtf8Bytes: Int = 4_096,
    val maxMimeTypeUtf8Bytes: Int = 512,
    val maxTotalNameUtf8Bytes: Long = 4L * 1_024 * 1_024,
    /** Sum of retained document-ID, display-name, and MIME UTF-8 bytes in one scan. */
    val maxTotalMetadataUtf8Bytes: Long = 8L * 1_024 * 1_024,
) {
    init {
        require(maxEntries in 1..100_000)
        require(maxDirectories in 1..25_000)
        require(maxDepth in 0..128)
        require(maxNameUtf8Bytes in 1..4_096)
        require(maxDocumentIdUtf8Bytes in 1..16_384)
        require(maxMimeTypeUtf8Bytes in 1..4_096)
        require(maxTotalNameUtf8Bytes in 1..(16L * 1_024 * 1_024))
        require(maxTotalMetadataUtf8Bytes in 1..(32L * 1_024 * 1_024))
    }
}

internal enum class SafSyncInventoryFailure {
    INVALID_TREE_URI,
    ACCESS_DENIED,
    CANCELLED,
    PROVIDER_QUERY_FAILED,
    NULL_CURSOR,
    LOADING_CURSOR,
    ERROR_CURSOR,
    MALFORMED_ROW,
    PARTIAL_DOCUMENT,
    VIRTUAL_DOCUMENT,
    ROOT_NOT_DIRECTORY,
    ENTRY_LIMIT,
    DIRECTORY_LIMIT,
    DEPTH_LIMIT,
    NAME_LIMIT,
    DOCUMENT_ID_LIMIT,
    MIME_TYPE_LIMIT,
    METADATA_LIMIT,
    DUPLICATE_DOCUMENT_ID,
    DOCUMENT_CYCLE,
    NON_PORTABLE_NAME,
    PORTABLE_NAME_COLLISION,
    UNSTABLE_OBSERVATION,
}

internal class SafSyncInventoryException(
    val reason: SafSyncInventoryFailure,
) : Exception("SAF metadata observation failed: ${reason.name}")

/** Direct ContentResolver entry point. No document content is opened or read. */
internal object SafStrictQuery {
    fun observeMetadata(
        resolver: ContentResolver,
        treeUri: Uri,
        cancellationSignal: CancellationSignal = CancellationSignal(),
        limits: SafSyncInventoryLimits = SafSyncInventoryLimits(),
    ): SafSyncMetadataObservation {
        if (!DocumentsContract.isTreeUri(treeUri) || treeUri.scheme != ContentResolver.SCHEME_CONTENT) {
            throw SafSyncInventoryException(SafSyncInventoryFailure.INVALID_TREE_URI)
        }
        val rootId = try {
            DocumentsContract.getTreeDocumentId(treeUri)
        } catch (_: RuntimeException) {
            throw SafSyncInventoryException(SafSyncInventoryFailure.INVALID_TREE_URI)
        }
        val source = ContentResolverMetadataSource(resolver, treeUri, cancellationSignal)
        return SafSyncInventory(source, limits).observe(rootId)
    }
}

internal data class SafRawMetadata(
    val documentId: String,
    val displayName: String,
    val mimeType: String,
    val flags: Int,
    val sizeBytes: Long?,
    val lastModifiedMillis: Long?,
)

/** Test seam for traversal logic; production uses ContentResolverMetadataSource below. */
internal interface SafMetadataSource {
    fun throwIfCancelled()
    fun queryDocument(documentId: String): SafRawMetadata
    fun queryChildren(parentDocumentId: String, consume: (SafRawMetadata) -> Unit)
}

internal class SafSyncInventory(
    private val source: SafMetadataSource,
    private val limits: SafSyncInventoryLimits,
) {
    fun observe(rootDocumentId: String): SafSyncMetadataObservation {
        validateOpaqueId(rootDocumentId)
        val first = scan(rootDocumentId)
        source.throwIfCancelled()
        val second = scan(rootDocumentId)
        source.throwIfCancelled()
        if (!first.sameMetadata(second)) {
            fail(SafSyncInventoryFailure.UNSTABLE_OBSERVATION)
        }
        return SafSyncMetadataObservation(rootDocumentId, first.entries)
    }

    private fun scan(rootDocumentId: String): Scan {
        source.throwIfCancelled()
        val root = source.queryDocument(rootDocumentId)
        val budget = ScanBudget()
        budget.charge(root, validatePortableName = false)
        if (root.documentId != rootDocumentId) fail(SafSyncInventoryFailure.MALFORMED_ROW)
        if (root.mimeType != DocumentsContract.Document.MIME_TYPE_DIR) {
            fail(SafSyncInventoryFailure.ROOT_NOT_DIRECTORY)
        }

        val entries = ArrayList<SafSyncMetadataEntry>()
        val seen = hashSetOf(rootDocumentId)
        var directoryCount = 1

        fun visit(parentId: String, depth: Int, ancestors: Set<String>) {
            source.throwIfCancelled()
            if (directoryCount > limits.maxDirectories) fail(SafSyncInventoryFailure.DIRECTORY_LIMIT)

            val children = ArrayList<SafRawMetadata>()
            val idsInQuery = hashSetOf<String>()
            source.queryChildren(parentId) { child ->
                source.throwIfCancelled()
                budget.reserveEntry()
                budget.charge(child, validatePortableName = true)
                if (!idsInQuery.add(child.documentId)) {
                    fail(SafSyncInventoryFailure.DUPLICATE_DOCUMENT_ID)
                }
                children += child
            }
            source.throwIfCancelled()

            val portableNames = hashSetOf<String>()
            children.sortedBy { it.documentId }.forEach { child ->
                source.throwIfCancelled()
                if (depth > limits.maxDepth) fail(SafSyncInventoryFailure.DEPTH_LIMIT)
                if (child.documentId in ancestors) fail(SafSyncInventoryFailure.DOCUMENT_CYCLE)
                if (!seen.add(child.documentId)) fail(SafSyncInventoryFailure.DUPLICATE_DOCUMENT_ID)

                val collisionKey = preliminaryPortableCollisionKey(child.displayName)
                if (!portableNames.add(collisionKey)) {
                    fail(SafSyncInventoryFailure.PORTABLE_NAME_COLLISION)
                }

                val kind = if (child.mimeType == DocumentsContract.Document.MIME_TYPE_DIR) {
                    SafSyncMetadataKind.DIRECTORY
                } else {
                    SafSyncMetadataKind.FILE
                }
                entries += SafSyncMetadataEntry(
                    documentId = child.documentId,
                    parentDocumentId = parentId,
                    displayName = child.displayName,
                    kind = kind,
                    mimeType = child.mimeType,
                    flags = child.flags,
                    sizeBytes = child.sizeBytes,
                    lastModifiedMillis = child.lastModifiedMillis,
                    depth = depth,
                )
                if (kind == SafSyncMetadataKind.DIRECTORY) {
                    directoryCount += 1
                    visit(child.documentId, depth + 1, ancestors + child.documentId)
                }
            }
        }

        visit(rootDocumentId, 1, setOf(rootDocumentId))
        entries.sortWith(compareBy({ it.documentId }, { it.parentDocumentId }))
        return Scan(root, entries)
    }

    private fun validateDocumentShape(document: SafRawMetadata) {
        if (document.flags and DocumentsContract.Document.FLAG_PARTIAL != 0) {
            fail(SafSyncInventoryFailure.PARTIAL_DOCUMENT)
        }
        if (document.flags and DocumentsContract.Document.FLAG_VIRTUAL_DOCUMENT != 0) {
            fail(SafSyncInventoryFailure.VIRTUAL_DOCUMENT)
        }
        if (document.mimeType.isEmpty()) fail(SafSyncInventoryFailure.MALFORMED_ROW)
        if (document.sizeBytes != null && document.sizeBytes < 0) {
            fail(SafSyncInventoryFailure.MALFORMED_ROW)
        }
        if (document.lastModifiedMillis != null && document.lastModifiedMillis < 0) {
            fail(SafSyncInventoryFailure.MALFORMED_ROW)
        }
    }

    private fun validateOpaqueId(documentId: String): Int {
        if (documentId.isEmpty()) fail(SafSyncInventoryFailure.MALFORMED_ROW)
        val bytes = strictUtf8Length(documentId, SafSyncInventoryFailure.DOCUMENT_ID_LIMIT)
        if (bytes > limits.maxDocumentIdUtf8Bytes) {
            fail(SafSyncInventoryFailure.DOCUMENT_ID_LIMIT)
        }
        return bytes
    }

    private fun validateMimeType(mimeType: String): Int {
        if (mimeType.isEmpty()) fail(SafSyncInventoryFailure.MALFORMED_ROW)
        val bytes = strictUtf8Length(mimeType, SafSyncInventoryFailure.MIME_TYPE_LIMIT)
        if (bytes > limits.maxMimeTypeUtf8Bytes) fail(SafSyncInventoryFailure.MIME_TYPE_LIMIT)
        return bytes
    }

    private fun validatePreliminaryPortableName(name: String): Int {
        if (name.isEmpty() || name == "." || name == "..") {
            fail(SafSyncInventoryFailure.NON_PORTABLE_NAME)
        }
        val byteCount = strictUtf8Length(name, SafSyncInventoryFailure.NON_PORTABLE_NAME)
        if (byteCount > limits.maxNameUtf8Bytes) fail(SafSyncInventoryFailure.NAME_LIMIT)
        if (Normalizer.normalize(name, Normalizer.Form.NFC) != name) {
            fail(SafSyncInventoryFailure.NON_PORTABLE_NAME)
        }
        if (name.last() == '.' || name.last() == ' ') {
            fail(SafSyncInventoryFailure.NON_PORTABLE_NAME)
        }
        if (name.any { character ->
                character == '/' || character == '\\' || character == '\u0000' ||
                    character in WINDOWS_FORBIDDEN || Character.isISOControl(character)
            }
        ) {
            fail(SafSyncInventoryFailure.NON_PORTABLE_NAME)
        }
        val stem = name.substringBefore('.').lowercase(Locale.ROOT)
        if (stem in WINDOWS_RESERVED_NAMES) fail(SafSyncInventoryFailure.NON_PORTABLE_NAME)
        return byteCount
    }

    private fun addBounded(current: Long, increment: Long, limit: Long, failure: SafSyncInventoryFailure): Long {
        val result = current + increment
        if (result < current || result > limit) fail(failure)
        return result
    }

    private fun Scan.sameMetadata(other: Scan): Boolean =
        root == other.root &&
            entries.size == other.entries.size &&
            entries.indices.all { entries[it].sameMetadata(other.entries[it]) }

    private data class Scan(val root: SafRawMetadata, val entries: List<SafSyncMetadataEntry>)

    private inner class ScanBudget {
        private var entryCount = 0
        private var totalNameBytes = 0L
        private var totalMetadataBytes = 0L

        fun reserveEntry() {
            entryCount += 1
            if (entryCount > limits.maxEntries) fail(SafSyncInventoryFailure.ENTRY_LIMIT)
        }

        fun charge(document: SafRawMetadata, validatePortableName: Boolean) {
            validateDocumentShape(document)
            val idBytes = validateOpaqueId(document.documentId)
            val nameBytes = if (validatePortableName) {
                validatePreliminaryPortableName(document.displayName)
            } else {
                val bytes = strictUtf8Length(document.displayName, SafSyncInventoryFailure.NAME_LIMIT)
                if (bytes > limits.maxNameUtf8Bytes) fail(SafSyncInventoryFailure.NAME_LIMIT)
                bytes
            }
            val mimeBytes = validateMimeType(document.mimeType)
            totalNameBytes = addBounded(
                totalNameBytes,
                nameBytes.toLong(),
                limits.maxTotalNameUtf8Bytes,
                SafSyncInventoryFailure.NAME_LIMIT,
            )
            totalMetadataBytes = addBounded(
                totalMetadataBytes,
                idBytes.toLong() + nameBytes + mimeBytes,
                limits.maxTotalMetadataUtf8Bytes,
                SafSyncInventoryFailure.METADATA_LIMIT,
            )
        }
    }

    private fun fail(reason: SafSyncInventoryFailure): Nothing = throw SafSyncInventoryException(reason)

    private fun strictUtf8Length(value: String, invalid: SafSyncInventoryFailure): Int {
        if (value.length > 16_384) fail(invalid)
        return try {
            val encoder = StandardCharsets.UTF_8.newEncoder()
                .onMalformedInput(CodingErrorAction.REPORT)
                .onUnmappableCharacter(CodingErrorAction.REPORT)
            encoder.encode(java.nio.CharBuffer.wrap(value)).remaining()
        } catch (_: CharacterCodingException) {
            fail(invalid)
        }
    }

    /**
     * Conservative early warning only. Locale.ROOT lowercase is not Unicode case folding and must
     * never replace the Rust SyncPath canonicalization required before an operation is authored.
     */
    private fun preliminaryPortableCollisionKey(name: String): String = name.lowercase(Locale.ROOT)

    private companion object {
        val WINDOWS_FORBIDDEN = setOf('<', '>', ':', '"', '|', '?', '*')
        val WINDOWS_RESERVED_NAMES = buildSet {
            addAll(listOf("con", "prn", "aux", "nul"))
            (1..9).forEach { index ->
                add("com$index")
                add("lpt$index")
            }
        }
    }
}

private class ContentResolverMetadataSource(
    private val resolver: ContentResolver,
    private val treeUri: Uri,
    private val cancellationSignal: CancellationSignal,
) : SafMetadataSource {
    override fun throwIfCancelled() {
        try {
            cancellationSignal.throwIfCanceled()
        } catch (_: OperationCanceledException) {
            throw SafSyncInventoryException(SafSyncInventoryFailure.CANCELLED)
        }
    }

    override fun queryDocument(documentId: String): SafRawMetadata {
        val uri = DocumentsContract.buildDocumentUriUsingTree(treeUri, documentId)
        return query(uri) { cursor ->
            if (!cursor.moveToFirst()) fail(SafSyncInventoryFailure.MALFORMED_ROW)
            val document = readRow(cursor)
            if (cursor.moveToNext()) fail(SafSyncInventoryFailure.MALFORMED_ROW)
            document
        }
    }

    override fun queryChildren(parentDocumentId: String, consume: (SafRawMetadata) -> Unit) {
        val uri = DocumentsContract.buildChildDocumentsUriUsingTree(treeUri, parentDocumentId)
        query(uri) { cursor ->
            while (cursor.moveToNext()) {
                throwIfCancelled()
                consume(readRow(cursor))
            }
            throwIfCancelled()
        }
    }

    private fun <T> query(uri: Uri, block: (Cursor) -> T): T {
        throwIfCancelled()
        try {
            val cursor = resolver.query(uri, PROJECTION, null, null, null, cancellationSignal)
                ?: fail(SafSyncInventoryFailure.NULL_CURSOR)
            cursor.use {
                validateExtras(it)
                val result = block(it)
                throwIfCancelled()
                validateExtras(it)
                return result
            }
        } catch (error: SafSyncInventoryException) {
            throw error
        } catch (_: OperationCanceledException) {
            fail(SafSyncInventoryFailure.CANCELLED)
        } catch (_: SecurityException) {
            fail(SafSyncInventoryFailure.ACCESS_DENIED)
        } catch (_: RuntimeException) {
            fail(SafSyncInventoryFailure.PROVIDER_QUERY_FAILED)
        }
    }

    private fun validateExtras(cursor: Cursor) {
        val extras = cursor.extras
        if (extras.getBoolean(DocumentsContract.EXTRA_LOADING, false)) {
            fail(SafSyncInventoryFailure.LOADING_CURSOR)
        }
        if (extras.containsKey(DocumentsContract.EXTRA_ERROR)) {
            fail(SafSyncInventoryFailure.ERROR_CURSOR)
        }
    }

    private fun readRow(cursor: Cursor): SafRawMetadata {
        fun requiredString(column: String): String {
            val index = cursor.getColumnIndex(column)
            if (index < 0 || cursor.isNull(index)) fail(SafSyncInventoryFailure.MALFORMED_ROW)
            return cursor.getString(index) ?: fail(SafSyncInventoryFailure.MALFORMED_ROW)
        }

        fun requiredInt(column: String): Int {
            val index = cursor.getColumnIndex(column)
            if (index < 0 || cursor.isNull(index)) fail(SafSyncInventoryFailure.MALFORMED_ROW)
            return cursor.getInt(index)
        }

        fun optionalLong(column: String): Long? {
            val index = cursor.getColumnIndex(column)
            return if (index < 0 || cursor.isNull(index)) null else cursor.getLong(index)
        }

        return SafRawMetadata(
            documentId = requiredString(DocumentsContract.Document.COLUMN_DOCUMENT_ID),
            displayName = requiredString(DocumentsContract.Document.COLUMN_DISPLAY_NAME),
            mimeType = requiredString(DocumentsContract.Document.COLUMN_MIME_TYPE),
            flags = requiredInt(DocumentsContract.Document.COLUMN_FLAGS),
            sizeBytes = optionalLong(DocumentsContract.Document.COLUMN_SIZE),
            lastModifiedMillis = optionalLong(DocumentsContract.Document.COLUMN_LAST_MODIFIED),
        )
    }

    private fun fail(reason: SafSyncInventoryFailure): Nothing = throw SafSyncInventoryException(reason)

    private companion object {
        val PROJECTION = arrayOf(
            DocumentsContract.Document.COLUMN_DOCUMENT_ID,
            DocumentsContract.Document.COLUMN_DISPLAY_NAME,
            DocumentsContract.Document.COLUMN_MIME_TYPE,
            DocumentsContract.Document.COLUMN_FLAGS,
            DocumentsContract.Document.COLUMN_SIZE,
            DocumentsContract.Document.COLUMN_LAST_MODIFIED,
        )
    }
}
