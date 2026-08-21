package life.michaelwong.covalent.data

import android.content.Context
import android.net.Uri
import android.os.ParcelFileDescriptor
import android.util.Base64
import androidx.documentfile.provider.DocumentFile
import java.io.BufferedInputStream
import java.io.BufferedOutputStream
import java.io.IOException
import java.io.InputStream
import java.io.InterruptedIOException
import java.io.OutputStream
import java.net.HttpURLConnection
import java.nio.ByteBuffer
import java.security.DigestOutputStream
import java.security.MessageDigest
import java.util.zip.ZipEntry
import java.util.zip.ZipInputStream
import java.util.zip.ZipOutputStream
import life.michaelwong.covalent.model.RestorePlanReference
import life.michaelwong.covalent.model.RestorePreviewEntry
import life.michaelwong.covalent.model.TargetInventoryDraft
import life.michaelwong.covalent.model.TargetInventoryEntry
import org.json.JSONObject

/**
 * Streams Storage Access Framework trees through real OS file descriptors.
 * Content URIs remain inside Android and are never serialized into daemon path fields.
 */
class SafTransferBridge(private val node: CovalentNodeClient = CovalentNodeClient()) {
    /** Captures one stable, path-sorted SAF tree inventory without reading file contents. */
    fun targetInventory(context: Context, targetTree: Uri): TargetInventoryDraft {
        val root = DocumentFile.fromTreeUri(context, targetTree)
            ?.takeIf { it.exists() && it.isDirectory }
            ?: error("The selected restore folder is unavailable. Choose it again.")
        val rootBefore = snapshot(root)
        val seenDocuments = mutableSetOf<String>()
        val seenPaths = mutableSetOf<String>()
        val entries = mutableListOf<TargetInventoryEntry>()
        var totalBytes = 0L

        fun inventoryDirectory(directory: DocumentFile, parentPath: String, depth: Int) {
            check(depth <= MAX_TREE_DEPTH) { "The restore folder is nested too deeply." }
            check(seenDocuments.add(directory.uri.toString())) {
                "The document provider returned a folder cycle."
            }
            directory.listFiles().forEach { child ->
                ensureTransferActive()
                val before = snapshot(child)
                val name = SafArchivePath.requireComponent(before.name)
                val path = if (parentPath.isEmpty()) name else "$parentPath/$name"
                SafArchivePath.parse(path, before.isDirectory)
                check(seenPaths.add(path)) { "The document provider returned duplicate path $path." }
                check(entries.size < MAX_TARGET_INVENTORY_ENTRIES) {
                    "The restore folder contains too many entries."
                }
                if (!before.isDirectory) totalBytes = Math.addExact(totalBytes, before.length)
                entries += TargetInventoryEntry(
                    path = path,
                    kind = if (before.isDirectory) "directory" else "file",
                    length = before.length,
                    modifiedAtUnixMs = before.modifiedAtUnixMs,
                    identityToken = before.identityToken,
                )
                if (before.isDirectory) inventoryDirectory(child, path, depth + 1)
                check(snapshot(child) == before) {
                    "The restore folder changed while it was being inventoried."
                }
            }
        }

        inventoryDirectory(root, "", 0)
        check(snapshot(root) == rootBefore) { "The restore folder changed while it was being inventoried." }
        val sorted = entries.sortedWith { left, right -> compareUtf8(left.path, right.path) }
        return TargetInventoryDraft(
            rootIdentity = safRootIdentity(targetTree.toString(), rootBefore.identityToken),
            totalBytes = totalBytes,
            entries = sorted,
        )
    }

    fun createBackup(
        context: Context,
        baseUrl: String,
        token: String,
        sourceTree: Uri,
        metadata: JSONObject,
        onProgress: (completedBytes: Long, completedEntries: Long) -> Unit = { _, _ -> },
    ): ArchiveTransferResult {
        val versionedMetadata = JSONObject(metadata.toString()).put("protocolVersion", COVALENT_PROTOCOL_VERSION)
        val metadataHeader = Base64.encodeToString(
            versionedMetadata.toString().encodeToByteArray(),
            Base64.URL_SAFE or Base64.NO_WRAP or Base64.NO_PADDING,
        )
        val descriptor = describeArchive(context, sourceTree)
        var offset = 0L
        var attempt = 0
        var probedAfterInterruption = false
        while (attempt++ < MAX_ARCHIVE_UPLOAD_ATTEMPTS) {
            try {
                val connection = openArchiveUpload(baseUrl, token, metadataHeader, descriptor, offset)
                ZipOutputStream(BufferedOutputStream(SkippingOutputStream(connection.outputStream, offset), STREAM_BUFFER_BYTES)).use {
                    writeSourceTree(context, sourceTree, it, onProgress)
                }
                node.ensureSuccess(connection)
                requireAcknowledgementContract(connection)
                return ArchiveTransferResult(JSONObject(node.readResponse(connection)), acknowledgementRequired = true)
            } catch (error: NodeApiException) {
                val serverOffset = archiveUploadRetryOffset(error, descriptor.length) ?: throw error
                offset = serverOffset
            } catch (error: IOException) {
                // A broken connection has no durable-offset response. Probe offset zero once;
                // the node responds with its authoritative 409 offset without duplicating bytes.
                if (probedAfterInterruption) throw error
                probedAfterInterruption = true
                offset = 0L
            }
        }
        error("Archive upload did not converge on a durable offset.")
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
        val restoreRequest = transfer.getJSONObject("restoreRequest")
        val legacyPlan = restoreRequest.optJSONObject("plan")
        val storedReference = transfer.optJSONObject("planReference")
            ?.let(::restorePlanReferenceFromPersistence)
        val freshInventory: TargetInventoryDraft?
        val executionReference: RestorePlanReference?
        val expected: MutableMap<String, RestorePreviewEntry>
        val requestBody: JSONObject
        if (legacyPlan != null) {
            check(target.listFiles().isEmpty()) {
                "This older restore preview requires an empty folder. Preview again to restore safely into existing content."
            }
            freshInventory = null
            executionReference = null
            expected = signedLegacyEntries(legacyPlan)
            requestBody = restoreRequest
        } else {
            val reference = checkNotNull(storedReference) {
                "The saved restore request is incomplete. Preview the restore again."
            }
            check(reference.planId != null && reference.targetInventory != null) {
                "Preview this restore again so it is bound to the selected Android folder."
            }
            freshInventory = targetInventory(context, targetTree)
            val uploaded = node.uploadTargetInventory(baseUrl, token, reference.jobId, freshInventory)
            val rebound = node.previewArchiveRestore(
                baseUrl,
                token,
                JSONObject()
                    .put("backupId", reference.backupId)
                    .put("snapshotId", reference.snapshotId)
                    .put("conflictPolicy", reference.conflictPolicy)
                    .put("jobId", reference.jobId)
                    .put("targetInventoryId", uploaded.inventoryId),
            )
            check(sameSignedRestorePlan(reference, rebound.reference)) {
                "The restore folder changed after preview. Refresh the preview before restoring."
            }
            val binding = checkNotNull(rebound.reference.targetInventory)
            check(binding.schemaVersion == uploaded.schemaVersion &&
                binding.rootIdentity == uploaded.rootIdentity &&
                binding.entryCount == uploaded.entryCount &&
                binding.totalBytes == uploaded.totalBytes &&
                binding.inventoryDigest == uploaded.inventoryDigest
            ) { "The signed restore plan does not match the current Android folder inventory." }
            val planEntries = node.restorePlanEntries(baseUrl, token, rebound)
            expected = signedArchiveEntries(planEntries)
            executionReference = rebound.reference
            requestBody = JSONObject().put("planId", checkNotNull(reference.planId))
        }
        val request = requestBody.toString().encodeToByteArray()
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
            if (executionReference != null) {
                check(connection.getHeaderField(RESTORE_PLAN_ID_HEADER) == executionReference.planId) {
                    "The restore response does not match its durable plan ID."
                }
                check(connection.getHeaderField(RESTORE_PLAN_DIGEST_HEADER) == executionReference.planDigest) {
                    "The restore response does not match its signed plan digest."
                }
            }
            if (freshInventory != null) {
                check(targetInventory(context, targetTree) == freshInventory) {
                    "The restore folder changed after preview. Refresh the preview before restoring."
                }
            } else {
                check(target.listFiles().isEmpty()) {
                    "The restore folder changed after preview. Choose an empty folder and preview again."
                }
            }
            val created = mutableListOf<DocumentFile>()
            try {
                ZipInputStream(BufferedInputStream(connection.inputStream, STREAM_BUFFER_BYTES)).use { archive ->
                    extractRestoreArchive(
                        context,
                        target,
                        archive,
                        expected,
                        freshInventory,
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
                        try {
                            writeZeroByteZipContent(archive)
                        } finally {
                            archive.closeEntry()
                        }
                        onProgress(totalBytes, entries.toLong())
                        writeDirectory(child, path, depth + 1)
                    }
                    child.isFile -> {
                        archive.putNextEntry(ZipEntry(path).apply { time = ZIP_EPOCH_MILLIS })
                        try {
                            // A write call is deliberate even for a zero-byte SAF document: ZIP
                            // entries represent empty files/directories without changing their data.
                            writeZeroByteZipContent(archive)
                            val descriptor = context.contentResolver.openFileDescriptor(child.uri, "r")
                                ?: error("The document provider could not open $path.")
                            ParcelFileDescriptor.AutoCloseInputStream(descriptor).use { input ->
                                totalBytes = copySource(input, archive, totalBytes) { bytes ->
                                    onProgress(bytes, entries.toLong())
                                }
                            }
                        } finally {
                            archive.closeEntry()
                        }
                        onProgress(totalBytes, entries.toLong())
                    }
                    else -> error("The document provider returned an unsupported entry at $path.")
                }
            }
        }

        writeDirectory(root, "", 0)
    }

    private fun describeArchive(context: Context, sourceTree: Uri): ArchiveDescriptor {
        val digest = MessageDigest.getInstance("SHA-256")
        val counter = CountingOutputStream()
        ZipOutputStream(BufferedOutputStream(DigestOutputStream(counter, digest), STREAM_BUFFER_BYTES)).use {
            writeSourceTree(context, sourceTree, it) { _, _ -> }
        }
        return ArchiveDescriptor(counter.count, digest.digest().joinToString("") { "%02x".format(it.toInt() and 0xff) })
    }

    private fun openArchiveUpload(baseUrl: String, token: String, metadata: String, archive: ArchiveDescriptor, offset: Long): HttpURLConnection =
        node.openConnection(baseUrl, "/api/v1/backups/archive", "POST", token, "application/json", TRANSFER_TIMEOUT_MILLIS).apply {
            doOutput = true
            setFixedLengthStreamingMode(Math.subtractExact(archive.length, offset))
            setRequestProperty("Content-Type", "application/vnd.covalent.backup+zip")
            setRequestProperty(ARCHIVE_METADATA_HEADER, metadata)
            setRequestProperty(ARCHIVE_UPLOAD_OFFSET_HEADER, offset.toString())
            setRequestProperty(ARCHIVE_UPLOAD_LENGTH_HEADER, archive.length.toString())
            setRequestProperty(ARCHIVE_UPLOAD_DIGEST_HEADER, archive.sha256)
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
        expected: MutableMap<String, RestorePreviewEntry>,
        expectedInventory: TargetInventoryDraft?,
        created: MutableList<DocumentFile>,
        onProgress: (Long, Long) -> Unit,
    ) {
        val expectedArchiveEntries = expected.size.toLong()
        check(expectedArchiveEntries <= MAX_ARCHIVE_ENTRIES.toLong()) {
            "The restore archive contains too many write actions for Android."
        }
        val knownDirectories = mutableMapOf("" to snapshot(root).identityToken)
        expectedInventory?.entries
            ?.filter { it.kind == "directory" }
            ?.forEach { knownDirectories[it.path] = it.identityToken }
        val seen = mutableSetOf<String>()
        var entries = 0
        var totalBytes = 0L
        while (true) {
            val entry = archive.nextEntry ?: break
            val path = SafArchivePath.parse(entry.name, entry.isDirectory)
            check(seen.add(path.canonical)) { "The restore archive contains a duplicate path." }
            val planned = expected.remove(path.canonical)
                ?: error("The restore archive contains an unsigned path.")
            check((planned.kind == "directory") == path.isDirectory) {
                "The restore archive does not match its signed preview."
            }
            entries += 1
            check(entries <= MAX_ARCHIVE_ENTRIES) { "The restore archive contains too many entries." }
            val parentComponents = path.components.dropLast(1)
            val parentPath = parentComponents.joinToString("/")
            val parent = resolveDirectory(root, parentComponents, knownDirectories)
            val name = path.components.last()
            if (path.isDirectory) {
                check(planned.action == "create_directory") {
                    "The restore archive included a directory that should remain unchanged."
                }
                val existing = parent.findFile(name)
                check(existing == null) { "The restore folder changed after preview." }
                val directory = checkNotNull(parent.createDirectory(name)) {
                    "The document provider could not create ${path.canonical}."
                }
                check(directory.name == name && directory.isDirectory) {
                    directory.delete()
                    "The document provider changed the restore directory name."
                }
                created += directory
                knownDirectories[path.canonical] = snapshot(directory).identityToken
                knownDirectories[parentPath] = snapshot(parent).identityToken
                onProgress(totalBytes, entries.toLong())
                archive.closeEntry()
                continue
            }

            check(planned.action == "create_file" || planned.action == "rename_file") {
                "The restore archive included a file that should remain unchanged."
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
            knownDirectories[parentPath] = snapshot(parent).identityToken
            archive.closeEntry()
            onProgress(totalBytes, entries.toLong())
        }
        check(entries.toLong() == expectedArchiveEntries) {
            "The restore archive entry count does not match its signed plan."
        }
        check(expected.isEmpty()) { "The restore archive omitted a signed path." }
    }

    private fun signedLegacyEntries(plan: JSONObject): MutableMap<String, RestorePreviewEntry> {
        check(plan.getString("conflictPolicy") == "fail") {
            "Streamed restores require the fail-on-conflict policy and an empty folder."
        }
        val expected = linkedMapOf<String, RestorePreviewEntry>()
        val entries = plan.getJSONArray("entries")
        for (index in 0 until entries.length()) {
            val entry = entries.getJSONObject(index)
            val directory = entry.getString("kind") == "directory"
            val action = entry.getString("action")
            check(action == if (directory) "create_directory" else "create_file") {
                "The signed restore plan could modify existing content."
            }
            val path = SafArchivePath.parse(entry.getString("destinationPath"), directory)
            val planned = RestorePreviewEntry(
                sourcePath = entry.optString("sourcePath", path.canonical),
                destinationPath = path.canonical,
                kind = if (directory) "directory" else "file",
                action = action,
            )
            check(expected.put(path.canonical, planned) == null) {
                "The signed restore plan contains a duplicate path."
            }
        }
        return expected
    }

    private fun signedArchiveEntries(
        entries: List<RestorePreviewEntry>,
    ): MutableMap<String, RestorePreviewEntry> {
        val expected = linkedMapOf<String, RestorePreviewEntry>()
        val seen = mutableSetOf<String>()
        entries.forEach { planned ->
            check(isSafeSafRestoreAction(planned.kind, planned.action)) {
                "The signed restore plan contains an action Android cannot apply safely."
            }
            val directory = planned.kind == "directory"
            val path = SafArchivePath.parse(planned.destinationPath, directory)
            check(seen.add(path.canonical)) { "The signed restore plan contains a duplicate path." }
            when (planned.action) {
                "create_directory", "create_file", "rename_file" -> expected[path.canonical] = planned
                "keep_directory", "skip_file" -> Unit
                else -> error("The signed restore plan contains an unsupported action.")
            }
        }
        return expected
    }

    private fun resolveDirectory(
        root: DocumentFile,
        components: List<String>,
        knownDirectories: Map<String, String>,
    ): DocumentFile {
        check(snapshot(root).identityToken == knownDirectories[""]) {
            "The restore folder changed while files were being applied."
        }
        var current = root
        val traversed = mutableListOf<String>()
        components.forEach { name ->
            traversed += name
            val path = traversed.joinToString("/")
            val existing = current.findFile(name)
                ?: error("The restore archive omitted parent directory $name.")
            check(existing.isDirectory) { "A file blocks restore directory $name." }
            check(snapshot(existing).identityToken == knownDirectories[path]) {
                "The restore folder changed while files were being applied."
            }
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

    private fun snapshot(document: DocumentFile): SafDocumentSnapshot {
        check(document.exists()) { "The restore folder changed while it was being inspected." }
        val directory = document.isDirectory
        val file = document.isFile
        check(directory.xor(file)) { "The document provider returned an unsupported entry." }
        val length = if (file) document.length() else 0L
        check(length >= 0) { "The document provider returned an invalid file length." }
        val modified = document.lastModified().takeIf { it > 0 }
        val name = document.name
        val type = document.type
        val uri = document.uri.toString()
        val fields = listOf(
            "saf-document/v1",
            uri,
            name.orEmpty(),
            type.orEmpty(),
            if (directory) "directory" else "file",
            length.toString(),
            modified?.toString().orEmpty(),
        )
        return SafDocumentSnapshot(
            name = name,
            isDirectory = directory,
            length = length,
            modifiedAtUnixMs = modified,
            identityToken = "saf-sha256=${sha256Fields(fields)}",
        )
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
        const val ARCHIVE_UPLOAD_OFFSET_HEADER = "X-Covalent-Upload-Offset"
        const val ARCHIVE_UPLOAD_LENGTH_HEADER = "X-Covalent-Upload-Length"
        const val ARCHIVE_UPLOAD_DIGEST_HEADER = "X-Covalent-Upload-Digest"
        const val ARCHIVE_RESULT_HEADER = "X-Covalent-Restore-Result"
        const val RESTORE_PLAN_ID_HEADER = "X-Covalent-Restore-Plan-Id"
        const val RESTORE_PLAN_DIGEST_HEADER = "X-Covalent-Restore-Plan-Digest"
        const val JOB_ACK_REQUIRED_HEADER = "X-Covalent-Job-Ack-Required"
        const val STREAM_BUFFER_BYTES = 64 * 1_024
        const val TRANSFER_TIMEOUT_MILLIS = 24 * 60 * 60 * 1_000
        const val MAX_ARCHIVE_ENTRIES = 100_000
        const val MAX_TARGET_INVENTORY_ENTRIES = 250_000
        const val MAX_TREE_DEPTH = 128
        const val MAX_ARCHIVE_BYTES = 256L * 1_024 * 1_024 * 1_024
        const val ZIP_EPOCH_MILLIS = 315_532_800_000L
        const val COVALENT_PROTOCOL_VERSION = 1
        const val MAX_ARCHIVE_UPLOAD_ATTEMPTS = 4
    }
}

private data class ArchiveDescriptor(val length: Long, val sha256: String)

private data class SafDocumentSnapshot(
    val name: String?,
    val isDirectory: Boolean,
    val length: Long,
    val modifiedAtUnixMs: Long?,
    val identityToken: String,
)

internal fun safRootIdentity(treeUri: String, rootSnapshotToken: String): String {
    require(treeUri.isNotBlank() && rootSnapshotToken.isNotBlank())
    return "saf-root-sha256=${sha256Fields(listOf(treeUri, rootSnapshotToken))}"
}

private fun sha256Fields(values: List<String>): String {
    val digest = MessageDigest.getInstance("SHA-256")
    values.forEach { value ->
        val bytes = value.encodeToByteArray()
        digest.update(ByteBuffer.allocate(Long.SIZE_BYTES).putLong(bytes.size.toLong()).array())
        digest.update(bytes)
    }
    return digest.digest().joinToString("") { "%02x".format(it.toInt() and 0xff) }
}

/** Accept only a retryable, bounded server offset; identity and digest conflicts remain terminal. */
internal fun archiveUploadRetryOffset(error: NodeApiException, archiveLength: Long): Long? =
    error.uploadOffset?.takeIf { error.retryable && it in 0..archiveLength }

private class CountingOutputStream : OutputStream() {
    var count = 0L
        private set
    override fun write(value: Int) { count = Math.addExact(count, 1) }
    override fun write(bytes: ByteArray, offset: Int, length: Int) { count = Math.addExact(count, length.toLong()) }
}

private class SkippingOutputStream(private val delegate: OutputStream, private var remaining: Long) : OutputStream() {
    override fun write(value: Int) = write(byteArrayOf(value.toByte()), 0, 1)
    override fun write(bytes: ByteArray, offset: Int, length: Int) {
        var start = offset
        var size = length
        val skipped = minOf(remaining, size.toLong()).toInt()
        remaining -= skipped
        start += skipped
        size -= skipped
        if (size > 0) delegate.write(bytes, start, size)
    }
    override fun flush() = delegate.flush()
    override fun close() = delegate.close()
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

/** Preserves zero-byte file and directory semantics while completing a ZIP entry write contract. */
internal fun writeZeroByteZipContent(archive: ZipOutputStream) {
    archive.write(EMPTY_ZIP_CONTENT)
}

private val EMPTY_ZIP_CONTENT = ByteArray(0)

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
