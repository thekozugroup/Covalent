package life.michaelwong.covalent.data

import android.content.Context
import android.net.Uri
import android.os.ParcelFileDescriptor
import android.util.Base64
import androidx.documentfile.provider.DocumentFile
import java.io.BufferedInputStream
import java.io.BufferedOutputStream
import java.io.InputStream
import java.io.InterruptedIOException
import java.io.OutputStream
import java.net.HttpURLConnection
import java.util.zip.ZipEntry
import java.util.zip.ZipInputStream
import java.util.zip.ZipOutputStream
import org.json.JSONObject

/**
 * Streams Storage Access Framework trees through real OS file descriptors.
 * Content URIs remain inside Android and are never serialized into daemon path fields.
 */
class SafTransferBridge(private val node: CovalentNodeClient = CovalentNodeClient()) {
    fun createBackup(
        context: Context,
        baseUrl: String,
        token: String,
        sourceTree: Uri,
        metadata: JSONObject,
        onProgress: (completedBytes: Long, completedEntries: Long) -> Unit = { _, _ -> },
    ): ArchiveTransferResult {
        val versionedMetadata = JSONObject(metadata.toString()).put("protocolVersion", COVALENT_PROTOCOL_VERSION)
        val connection = node.openConnection(
            baseUrl = baseUrl,
            path = "/api/v1/backups/archive",
            method = "POST",
            token = token,
            accept = "application/json",
            readTimeoutMillis = TRANSFER_TIMEOUT_MILLIS,
        ).apply {
            doOutput = true
            setChunkedStreamingMode(STREAM_BUFFER_BYTES)
            setRequestProperty("Content-Type", "application/vnd.covalent.backup+zip")
            setRequestProperty(
                ARCHIVE_METADATA_HEADER,
                Base64.encodeToString(
                    versionedMetadata.toString().encodeToByteArray(),
                    Base64.URL_SAFE or Base64.NO_WRAP or Base64.NO_PADDING,
                ),
            )
        }
        try {
            ZipOutputStream(BufferedOutputStream(connection.outputStream, STREAM_BUFFER_BYTES)).use { archive ->
                writeSourceTree(context, sourceTree, archive, onProgress)
            }
            node.ensureSuccess(connection)
            requireAcknowledgementContract(connection)
            return ArchiveTransferResult(JSONObject(node.readResponse(connection)), acknowledgementRequired = true)
        } catch (error: Exception) {
            connection.disconnect()
            throw error
        }
    }

    fun restore(
        context: Context,
        baseUrl: String,
        token: String,
        targetTree: Uri,
        transfer: JSONObject,
        onProgress: (completedBytes: Long, completedEntries: Long) -> Unit = { _, _ -> },
    ): ArchiveTransferResult {
        val target = DocumentFile.fromTreeUri(context, targetTree)
            ?.takeIf { it.exists() && it.isDirectory }
            ?: error("The selected restore folder is unavailable. Choose it again.")
        check(target.listFiles().isEmpty()) {
            "Choose an empty restore folder so the signed no-write preview remains exact."
        }
        val restoreRequest = transfer.getJSONObject("restoreRequest")
        val legacyPlan = restoreRequest.optJSONObject("plan")
        val expected = legacyPlan?.let(::signedCreateEntries)
        val expectedTotalEntries = transfer.getLong("expectedTotalEntries")
        check(expectedTotalEntries in 0..MAX_ARCHIVE_ENTRIES.toLong()) {
            "The restore plan exceeds Android's streamed entry limit."
        }
        val expectedPlanId = transfer.optionalString("expectedPlanId")
        val expectedPlanDigest = transfer.getString("expectedPlanDigest")
        check((legacyPlan == null) == (expectedPlanId != null)) {
            "The saved restore request is incomplete. Preview the restore again."
        }
        val request = restoreRequest.toString().encodeToByteArray()
        val connection = node.openConnection(
            baseUrl = baseUrl,
            path = "/api/v1/restores/archive/execute",
            method = "POST",
            token = token,
            accept = "application/vnd.covalent.restore+zip",
            readTimeoutMillis = TRANSFER_TIMEOUT_MILLIS,
        ).apply {
            doOutput = true
            setFixedLengthStreamingMode(request.size)
            setRequestProperty("Content-Type", "application/json")
            outputStream.use { it.write(request) }
        }
        try {
            node.ensureSuccess(connection)
            requireAcknowledgementContract(connection)
            if (expectedPlanId != null) {
                check(connection.getHeaderField(RESTORE_PLAN_ID_HEADER) == expectedPlanId) {
                    "The restore response does not match its durable plan ID."
                }
                check(connection.getHeaderField(RESTORE_PLAN_DIGEST_HEADER) == expectedPlanDigest) {
                    "The restore response does not match its signed plan digest."
                }
            }
            check(target.listFiles().isEmpty()) {
                "The restore folder changed after preview. Choose an empty folder and preview again."
            }
            val created = mutableListOf<DocumentFile>()
            try {
                ZipInputStream(BufferedInputStream(connection.inputStream, STREAM_BUFFER_BYTES)).use { archive ->
                    extractRestoreArchive(
                        context,
                        target,
                        archive,
                        expected,
                        expectedTotalEntries,
                        created,
                        onProgress,
                    )
                }
            } catch (error: Exception) {
                created.asReversed().forEach { item ->
                    if (item.isFile || item.listFiles().isEmpty()) item.delete()
                }
                throw error
            }
            val result = connection.getHeaderField(ARCHIVE_RESULT_HEADER)
                ?: error("The node omitted the restore result contract.")
            return ArchiveTransferResult(
                body = JSONObject(
                    Base64.decode(result, Base64.URL_SAFE or Base64.NO_WRAP or Base64.NO_PADDING)
                        .decodeToString(),
                ),
                acknowledgementRequired = true,
            )
        } finally {
            connection.disconnect()
        }
    }

    private fun writeSourceTree(
        context: Context,
        treeUri: Uri,
        archive: ZipOutputStream,
        onProgress: (Long, Long) -> Unit,
    ) {
        val root = DocumentFile.fromTreeUri(context, treeUri)
            ?.takeIf { it.exists() && it.isDirectory }
            ?: error("The selected source folder is unavailable. Choose it again.")
        val seenDocuments = mutableSetOf<Uri>()
        val seenPaths = mutableSetOf<String>()
        var entries = 0
        var totalBytes = 0L

        fun writeDirectory(directory: DocumentFile, parentPath: String, depth: Int) {
            check(depth <= MAX_TREE_DEPTH) { "The selected folder is nested too deeply." }
            check(seenDocuments.add(directory.uri)) { "The document provider returned a folder cycle." }
            val children = directory.listFiles().sortedWith(
                compareBy<DocumentFile>({ it.name.orEmpty() }, { it.uri.toString() }),
            )
            children.forEach { child ->
                val name = SafArchivePath.requireComponent(child.name)
                val path = if (parentPath.isEmpty()) name else "$parentPath/$name"
                check(seenPaths.add(path)) { "The document provider returned duplicate path $path." }
                entries += 1
                check(entries <= MAX_ARCHIVE_ENTRIES) { "The selected folder contains too many entries." }
                when {
                    child.isDirectory -> {
                        archive.putNextEntry(ZipEntry("$path/").apply { time = ZIP_EPOCH_MILLIS })
                        archive.closeEntry()
                        onProgress(totalBytes, entries.toLong())
                        writeDirectory(child, path, depth + 1)
                    }
                    child.isFile -> {
                        archive.putNextEntry(ZipEntry(path).apply { time = ZIP_EPOCH_MILLIS })
                        val descriptor = context.contentResolver.openFileDescriptor(child.uri, "r")
                            ?: error("The document provider could not open $path.")
                        ParcelFileDescriptor.AutoCloseInputStream(descriptor).use { input ->
                            totalBytes = copySource(input, archive, totalBytes) { bytes ->
                                onProgress(bytes, entries.toLong())
                            }
                        }
                        archive.closeEntry()
                        onProgress(totalBytes, entries.toLong())
                    }
                    else -> error("The document provider returned an unsupported entry at $path.")
                }
            }
        }

        writeDirectory(root, "", 0)
    }

    private fun copySource(
        input: InputStream,
        output: OutputStream,
        currentTotal: Long,
        onProgress: (Long) -> Unit,
    ): Long {
        var total = currentTotal
        val buffer = ByteArray(STREAM_BUFFER_BYTES)
        while (true) {
            ensureTransferActive()
            val count = input.read(buffer)
            if (count < 0) break
            total = Math.addExact(total, count.toLong())
            check(total <= MAX_ARCHIVE_BYTES) { "The selected folder exceeds the transfer limit." }
            output.write(buffer, 0, count)
            onProgress(total)
        }
        return total
    }

    private fun extractRestoreArchive(
        context: Context,
        root: DocumentFile,
        archive: ZipInputStream,
        expected: MutableMap<String, Boolean>?,
        expectedTotalEntries: Long,
        created: MutableList<DocumentFile>,
        onProgress: (Long, Long) -> Unit,
    ) {
        val seen = mutableSetOf<String>()
        var entries = 0
        var totalBytes = 0L
        while (true) {
            val entry = archive.nextEntry ?: break
            val path = SafArchivePath.parse(entry.name, entry.isDirectory)
            check(seen.add(path.canonical)) { "The restore archive contains a duplicate path." }
            if (expected != null) {
                check(expected.remove(path.canonical) == path.isDirectory) {
                    "The restore archive does not match its signed no-write preview."
                }
            }
            entries += 1
            check(entries <= MAX_ARCHIVE_ENTRIES) { "The restore archive contains too many entries." }
            val parent = resolveDirectory(root, path.components.dropLast(1))
            val name = path.components.last()
            if (path.isDirectory) {
                val existing = parent.findFile(name)
                check(existing == null || existing.isDirectory) {
                    "A file blocks restore directory ${path.canonical}."
                }
                if (existing == null) {
                    val directory = checkNotNull(parent.createDirectory(name)) {
                        "The document provider could not create ${path.canonical}."
                    }
                    created += directory
                }
                onProgress(totalBytes, entries.toLong())
                archive.closeEntry()
                continue
            }

            val existing = parent.findFile(name)
            check(existing == null) { "The restore folder changed after preview." }
            val destination = createFile(parent, name)
            created += destination
            try {
                val descriptor = context.contentResolver.openFileDescriptor(destination.uri, "rwt")
                    ?: error("The document provider could not open ${path.canonical} for writing.")
                ParcelFileDescriptor.AutoCloseOutputStream(descriptor).use { output ->
                    totalBytes = copyEntry(archive, output, totalBytes) { bytes ->
                        onProgress(bytes, entries.toLong())
                    }
                    output.flush()
                }
            } catch (error: Exception) {
                destination.delete()
                throw error
            }
            archive.closeEntry()
            onProgress(totalBytes, entries.toLong())
        }
        check(entries.toLong() == expectedTotalEntries) {
            "The restore archive entry count does not match its signed plan."
        }
        check(expected == null || expected.isEmpty()) { "The restore archive omitted a signed path." }
    }

    private fun signedCreateEntries(plan: JSONObject): MutableMap<String, Boolean> {
        check(plan.getString("conflictPolicy") == "fail") {
            "Streamed restores require the fail-on-conflict policy and an empty folder."
        }
        val expected = linkedMapOf<String, Boolean>()
        val entries = plan.getJSONArray("entries")
        for (index in 0 until entries.length()) {
            val entry = entries.getJSONObject(index)
            val directory = entry.getString("kind") == "directory"
            val action = entry.getString("action")
            check(action == if (directory) "create_directory" else "create_file") {
                "The signed restore plan could modify existing content."
            }
            val path = SafArchivePath.parse(entry.getString("destinationPath"), directory)
            check(expected.put(path.canonical, directory) == null) {
                "The signed restore plan contains a duplicate path."
            }
        }
        return expected
    }

    private fun resolveDirectory(
        root: DocumentFile,
        components: List<String>,
    ): DocumentFile {
        var current = root
        components.forEach { name ->
            val existing = current.findFile(name)
                ?: error("The restore archive omitted parent directory $name.")
            check(existing.isDirectory) { "A file blocks restore directory $name." }
            current = existing
        }
        return current
    }

    private fun createFile(parent: DocumentFile, name: String): DocumentFile {
        val created = checkNotNull(parent.createFile("application/octet-stream", name)) {
            "The document provider could not create restore file $name."
        }
        check(created.name == name) {
            created.delete()
            "The document provider changed the restore file name."
        }
        return created
    }

    private fun copyEntry(
        input: ZipInputStream,
        output: OutputStream?,
        currentTotal: Long,
        onProgress: (Long) -> Unit,
    ): Long {
        var total = currentTotal
        val buffer = ByteArray(STREAM_BUFFER_BYTES)
        while (true) {
            ensureTransferActive()
            val count = input.read(buffer)
            if (count < 0) break
            total = Math.addExact(total, count.toLong())
            check(total <= MAX_ARCHIVE_BYTES) { "The restore archive exceeds the transfer limit." }
            output?.write(buffer, 0, count)
            onProgress(total)
        }
        return total
    }

    private fun ensureTransferActive() {
        if (Thread.currentThread().isInterrupted) {
            throw InterruptedIOException("Android stopped this transfer; it will resume from its pending request.")
        }
    }

    private fun requireAcknowledgementContract(connection: HttpURLConnection) {
        requireJobAcknowledgement(connection.getHeaderField(JOB_ACK_REQUIRED_HEADER))
    }

    private companion object {
        const val ARCHIVE_METADATA_HEADER = "X-Covalent-Archive-Metadata"
        const val ARCHIVE_RESULT_HEADER = "X-Covalent-Restore-Result"
        const val RESTORE_PLAN_ID_HEADER = "X-Covalent-Restore-Plan-Id"
        const val RESTORE_PLAN_DIGEST_HEADER = "X-Covalent-Restore-Plan-Digest"
        const val JOB_ACK_REQUIRED_HEADER = "X-Covalent-Job-Ack-Required"
        const val STREAM_BUFFER_BYTES = 64 * 1_024
        const val TRANSFER_TIMEOUT_MILLIS = 24 * 60 * 60 * 1_000
        const val MAX_ARCHIVE_ENTRIES = 100_000
        const val MAX_TREE_DEPTH = 128
        const val MAX_ARCHIVE_BYTES = 256L * 1_024 * 1_024 * 1_024
        const val ZIP_EPOCH_MILLIS = 315_532_800_000L
        const val COVALENT_PROTOCOL_VERSION = 1
    }
}

data class ArchiveTransferResult(
    val body: JSONObject,
    val acknowledgementRequired: Boolean,
)

internal fun requireJobAcknowledgement(value: String?) {
    check(value == "true") {
        "The node omitted the required retained-job acknowledgement contract."
    }
}

private fun JSONObject.optionalString(key: String): String? =
    if (has(key) && !isNull(key)) getString(key).takeIf(String::isNotBlank) else null

internal data class SafArchivePath(
    val components: List<String>,
    val isDirectory: Boolean,
) {
    val canonical: String = components.joinToString("/")

    companion object {
        fun parse(raw: String, isDirectory: Boolean): SafArchivePath {
            val value = if (isDirectory) raw.removeSuffix("/") else raw
            check(value.isNotEmpty() && value.encodeToByteArray().size <= 4_096) {
                "The restore archive contains an invalid path."
            }
            check(!value.startsWith('/') && '\\' !in value && '\u0000' !in value) {
                "The restore archive contains an unsafe path."
            }
            val components = value.split('/').onEach(::requireComponent)
            return SafArchivePath(components, isDirectory)
        }

        fun requireComponent(value: String?): String {
            val component = value ?: error("A document has no display name.")
            check(component.isNotEmpty() && component != "." && component != "..") {
                "Document names must be safe relative path components."
            }
            check('/' !in component && '\\' !in component && '\u0000' !in component) {
                "Document names may not contain path separators."
            }
            check(component.encodeToByteArray().size <= 255) {
                "A document name exceeds the portable path limit."
            }
            return component
        }
    }
}
