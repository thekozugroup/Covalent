package life.michaelwong.covalent.sync

import android.content.Context
import android.net.Uri
import android.os.Build
import android.os.CancellationSignal
import android.provider.DocumentsContract
import android.util.Base64
import java.io.BufferedInputStream
import java.io.BufferedOutputStream
import java.io.Closeable
import java.io.EOFException
import java.io.InputStream
import java.io.OutputStream
import java.net.InetAddress
import java.net.ServerSocket
import java.net.Socket
import java.net.URI
import java.nio.charset.CodingErrorAction
import java.nio.charset.StandardCharsets
import java.security.MessageDigest
import java.text.Normalizer
import java.time.Instant
import java.time.ZoneOffset
import java.time.format.DateTimeFormatter
import java.util.Locale
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicBoolean
import kotlin.math.min

internal class SafWebDavEndpoint(val port: Int, val username: String, val password: String) {
    override fun toString(): String = "SafWebDavEndpoint(port=$port, credentials=<redacted>)"
}

/** Minimal authenticated WebDAV adapter for one exact persisted SAF tree. */
internal class SafWebDavServer(
    context: Context,
    private val treeUri: Uri,
    private val username: String,
    private val password: String,
) : Closeable {
    private val resolver = context.applicationContext.contentResolver
    private val running = AtomicBoolean(false)
    private val clients = Executors.newFixedThreadPool(4) { runnable ->
        Thread(runnable, "covalent-saf-webdav-client").apply { isDaemon = true }
    }
    private val clientSockets = ConcurrentHashMap.newKeySet<Socket>()
    private var listener: ServerSocket? = null
    private var acceptThread: Thread? = null

    init {
        SafFolderGrantStore.requireTreeUri(treeUri)
        require(username.length in 16..128 && password.length in 24..256)
        require(username.none { it == ':' || it.isISOControl() } && password.none(Char::isISOControl))
    }

    @Synchronized
    fun start(): SafWebDavEndpoint {
        check(!running.get())
        val socket = ServerSocket(0, 8, InetAddress.getByName(LOOPBACK))
        listener = socket
        running.set(true)
        acceptThread = Thread({ accept(socket) }, "covalent-saf-webdav-accept").apply {
            isDaemon = true
            start()
        }
        return SafWebDavEndpoint(socket.localPort, username, password)
    }

    private fun accept(server: ServerSocket) {
        while (running.get()) {
            val socket = runCatching { server.accept() }.getOrNull() ?: break
            if (!socket.inetAddress.isLoopbackAddress) {
                socket.close()
                continue
            }
            clientSockets += socket
            runCatching {
                clients.execute {
                    try {
                        socket.use(::serve)
                    } finally {
                        clientSockets -= socket
                    }
                }
            }.onFailure {
                clientSockets -= socket
                socket.close()
            }
        }
    }

    private fun serve(socket: Socket) {
        socket.soTimeout = SOCKET_TIMEOUT_MILLIS
        val input = BufferedInputStream(socket.getInputStream(), BUFFER_BYTES)
        val output = BufferedOutputStream(socket.getOutputStream(), BUFFER_BYTES)
        val request = try {
            readRequest(input)
        } catch (_: Exception) {
            writeResponse(output, 400, "Bad Request")
            return
        }
        if (!authorized(request.headers["authorization"])) {
            writeResponse(
                output,
                401,
                "Unauthorized",
                headers = mapOf("WWW-Authenticate" to "Basic realm=\"Covalent SAF\", charset=\"UTF-8\""),
            )
            return
        }
        runCatching { dispatch(request, input, output) }.getOrElse { error ->
            val response = when (error) {
                is MissingDocument -> 404 to "Not Found"
                is Conflict -> 409 to "Conflict"
                is SecurityException, is SafSyncInventoryException -> 403 to "Forbidden"
                is IllegalArgumentException -> 400 to "Bad Request"
                else -> 500 to "Internal Server Error"
            }
            runCatching { writeResponse(output, response.first, response.second) }
        }
    }

    private fun dispatch(request: Request, input: InputStream, output: OutputStream) {
        val path = parsePath(request.target)
        when (request.method) {
            "OPTIONS" -> writeResponse(
                output,
                200,
                "OK",
                headers = mapOf("Allow" to "OPTIONS, PROPFIND, GET, HEAD, PUT, MKCOL, DELETE, MOVE", "DAV" to "1"),
            )
            "PROPFIND" -> propfind(path, request, output)
            "GET" -> readFile(path, request, output, includeBody = true)
            "HEAD" -> readFile(path, request, output, includeBody = false)
            "PUT" -> put(path, request, input, output)
            "MKCOL" -> makeCollection(path, output)
            "DELETE" -> delete(path, output)
            "MOVE" -> move(path, request, output)
            else -> writeResponse(output, 405, "Method Not Allowed")
        }
    }

    private fun propfind(path: List<String>, request: Request, output: OutputStream) {
        val tree = TreeSnapshot.observe(resolver, treeUri)
        val target = tree.require(path)
        val depth = request.headers["depth"] ?: "0"
        require(depth == "0" || depth == "1")
        val nodes = if (depth == "1" && target.directory) listOf(target) + tree.children(target.id) else listOf(target)
        val xml = buildString {
            append("<?xml version=\"1.0\" encoding=\"utf-8\"?><D:multistatus xmlns:D=\"DAV:\">")
            nodes.forEach { node ->
                append("<D:response><D:href>").append(xmlEscape(encodedPath(tree.path(node.id), node.directory))).append("</D:href>")
                append("<D:propstat><D:prop><D:displayname>").append(xmlEscape(node.name)).append("</D:displayname>")
                append("<D:resourcetype>")
                if (node.directory) append("<D:collection/>")
                append("</D:resourcetype>")
                if (!node.directory) append("<D:getcontentlength>").append(node.size ?: 0).append("</D:getcontentlength>")
                node.modified?.let {
                    append("<D:getlastmodified>").append(httpDate(it)).append("</D:getlastmodified>")
                }
                append("<D:getcontenttype>").append(xmlEscape(node.mimeType)).append("</D:getcontenttype>")
                append("</D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>")
            }
            append("</D:multistatus>")
        }.encodeToByteArray()
        writeResponse(output, 207, "Multi-Status", xml, mapOf("Content-Type" to "application/xml; charset=utf-8"))
    }

    private fun readFile(path: List<String>, request: Request, output: OutputStream, includeBody: Boolean) {
        val tree = TreeSnapshot.observe(resolver, treeUri)
        val target = tree.require(path)
        if (target.directory) throw Conflict()
        val size = target.size ?: throw Conflict()
        require(request.headers["range"] == null) { "Range requests are not supported." }
        val headers = linkedMapOf("Content-Type" to target.mimeType, "Content-Length" to size.toString())
        if (!includeBody) {
            writeHead(output, 200, "OK", headers)
            return
        }
        writeHead(output, 200, "OK", headers)
        resolver.openInputStream(tree.uri(target.id))?.use { source -> source.copyTo(output, BUFFER_BYTES) }
            ?: throw SecurityException()
        output.flush()
    }

    private fun put(path: List<String>, request: Request, input: InputStream, output: OutputStream) {
        require(path.isNotEmpty())
        val length = request.headers["content-length"]?.toLongOrNull()
            ?: throw IllegalArgumentException("A bounded upload length is required.")
        require(length in 0..MAX_FILE_BYTES && request.headers["transfer-encoding"] == null)
        val tree = TreeSnapshot.observe(resolver, treeUri)
        val parent = tree.require(path.dropLast(1))
        if (!parent.directory) throw Conflict()
        val existing = tree.find(path)
        if (existing?.directory == true) throw Conflict()
        val created = existing == null
        val targetUri = existing?.let { tree.uri(it.id) } ?: DocumentsContract.createDocument(
            resolver,
            tree.uri(parent.id),
            request.headers["content-type"]?.takeIf(::validMimeType) ?: "application/octet-stream",
            path.last(),
        ) ?: throw SecurityException()
        tree.requireContained(targetUri)
        try {
            resolver.openOutputStream(targetUri, "rwt")?.use { destination -> copyExactly(input, destination, length) }
                ?: throw SecurityException()
        } catch (error: Throwable) {
            if (created) runCatching { DocumentsContract.deleteDocument(resolver, targetUri) }
            throw error
        }
        writeResponse(output, if (created) 201 else 204, if (created) "Created" else "No Content")
    }

    private fun makeCollection(path: List<String>, output: OutputStream) {
        require(path.isNotEmpty())
        val tree = TreeSnapshot.observe(resolver, treeUri)
        if (tree.find(path) != null) throw Conflict()
        val parent = tree.require(path.dropLast(1))
        if (!parent.directory) throw Conflict()
        val created = DocumentsContract.createDocument(
            resolver,
            tree.uri(parent.id),
            DocumentsContract.Document.MIME_TYPE_DIR,
            path.last(),
        ) ?: throw SecurityException()
        tree.requireContained(created)
        writeResponse(output, 201, "Created")
    }

    private fun delete(path: List<String>, output: OutputStream) {
        require(path.isNotEmpty()) { "The grant root cannot be deleted." }
        val tree = TreeSnapshot.observe(resolver, treeUri)
        val target = tree.require(path)
        check(DocumentsContract.deleteDocument(resolver, tree.uri(target.id)))
        writeResponse(output, 204, "No Content")
    }

    private fun move(path: List<String>, request: Request, output: OutputStream) {
        require(path.isNotEmpty())
        val destination = request.headers["destination"] ?: throw IllegalArgumentException("Missing destination.")
        val destinationUri = URI(destination)
        require(
            destinationUri.scheme == "http" && destinationUri.host == LOOPBACK &&
                destinationUri.port == listener?.localPort && destinationUri.userInfo == null &&
                destinationUri.query == null && destinationUri.fragment == null,
        )
        val destinationPath = parsePath(destinationUri.rawPath)
        require(destinationPath.isNotEmpty())
        val tree = TreeSnapshot.observe(resolver, treeUri)
        if (tree.find(destinationPath) != null) throw Conflict()
        val source = tree.require(path)
        val sourceParent = tree.require(path.dropLast(1))
        val destinationParent = tree.require(destinationPath.dropLast(1))
        var moved = if (sourceParent.id == destinationParent.id) tree.uri(source.id) else {
            DocumentsContract.moveDocument(
                resolver,
                tree.uri(source.id),
                tree.uri(sourceParent.id),
                tree.uri(destinationParent.id),
            ) ?: throw SecurityException()
        }
        if (source.name != destinationPath.last()) {
            moved = DocumentsContract.renameDocument(resolver, moved, destinationPath.last())
                ?: throw SecurityException()
        }
        tree.requireContained(moved)
        writeResponse(output, 201, "Created")
    }

    private fun authorized(header: String?): Boolean {
        if (header == null || !header.startsWith("Basic ")) return false
        val supplied = runCatching { Base64.decode(header.removePrefix("Basic "), Base64.DEFAULT) }.getOrNull()
            ?: return false
        val expected = "$username:$password".encodeToByteArray()
        return try {
            MessageDigest.isEqual(supplied, expected)
        } finally {
            supplied.fill(0)
            expected.fill(0)
        }
    }

    @Synchronized
    override fun close() {
        if (!running.getAndSet(false)) return
        runCatching { listener?.close() }
        acceptThread?.interrupt()
        clientSockets.forEach { runCatching { it.close() } }
        clients.shutdownNow()
        listener = null
        acceptThread = null
    }

    private data class Request(val method: String, val target: String, val headers: Map<String, String>)

    private class MissingDocument : Exception()
    private class Conflict : Exception()

    private class TreeSnapshot private constructor(
        private val resolver: android.content.ContentResolver,
        private val treeUri: Uri,
        private val rootId: String,
        private val nodes: List<Node>,
    ) {
        data class Node(
            val id: String,
            val parentId: String?,
            val name: String,
            val directory: Boolean,
            val mimeType: String,
            val size: Long?,
            val modified: Long?,
        )

        fun find(path: List<String>): Node? {
            var node = nodes.single { it.id == rootId }
            path.forEach { name ->
                if (!node.directory) return null
                node = nodes.singleOrNull { it.parentId == node.id && it.name == name } ?: return null
            }
            return node
        }

        fun require(path: List<String>): Node = find(path) ?: throw MissingDocument()
        fun children(id: String): List<Node> = nodes.filter { it.parentId == id }.sortedBy { it.name }
        fun uri(id: String): Uri = DocumentsContract.buildDocumentUriUsingTree(treeUri, id)

        fun requireContained(uri: Uri) {
            val root = uri(rootId)
            require(isWithinGrantedTree(resolver, treeUri, root, uri))
        }

        fun path(id: String): List<String> {
            val result = ArrayDeque<String>()
            var node = nodes.single { it.id == id }
            while (node.id != rootId) {
                result.addFirst(node.name)
                node = nodes.single { it.id == node.parentId }
            }
            return result.toList()
        }

        companion object {
            fun observe(resolver: android.content.ContentResolver, treeUri: Uri): TreeSnapshot {
                val observation = SafStrictQuery.observeMetadata(resolver, treeUri, CancellationSignal())
                val root = Node(
                    observation.rootDocumentId,
                    null,
                    "",
                    true,
                    DocumentsContract.Document.MIME_TYPE_DIR,
                    null,
                    null,
                )
                val rootUri = DocumentsContract.buildDocumentUriUsingTree(treeUri, observation.rootDocumentId)
                val entries = observation.entries.map {
                    val documentUri = DocumentsContract.buildDocumentUriUsingTree(treeUri, it.documentId)
                    require(isWithinGrantedTree(resolver, treeUri, rootUri, documentUri))
                    Node(
                        it.documentId,
                        it.parentDocumentId,
                        it.displayName,
                        it.kind == SafSyncMetadataKind.DIRECTORY,
                        it.mimeType,
                        it.sizeBytes,
                        it.lastModifiedMillis,
                    )
                }
                return TreeSnapshot(resolver, treeUri, observation.rootDocumentId, listOf(root) + entries)
            }
        }
    }

    companion object {
        private const val LOOPBACK = "127.0.0.1"
        private const val MAX_HEADER_BYTES = 32 * 1024
        private const val MAX_FILE_BYTES = 256L * 1024 * 1024 * 1024
        private const val SOCKET_TIMEOUT_MILLIS = 60_000
        private const val BUFFER_BYTES = 64 * 1024
        private val HTTP_DATE = DateTimeFormatter.RFC_1123_DATE_TIME.withLocale(Locale.US).withZone(ZoneOffset.UTC)

        private fun isWithinGrantedTree(
            resolver: android.content.ContentResolver,
            treeUri: Uri,
            rootUri: Uri,
            documentUri: Uri,
        ): Boolean {
            val structurallyBound = runCatching {
                documentUri.scheme == android.content.ContentResolver.SCHEME_CONTENT &&
                    documentUri.authority == treeUri.authority &&
                    DocumentsContract.getTreeDocumentId(documentUri) ==
                    DocumentsContract.getTreeDocumentId(treeUri)
            }.getOrDefault(false)
            if (!structurallyBound) return false
            // API 26-28 has no ContentResolver descendant query. Every URI on those
            // releases is constructed from the twice-observed traversal rooted at
            // this exact grant (or is a provider result under that grant).
            return Build.VERSION.SDK_INT < Build.VERSION_CODES.Q ||
                DocumentsContract.isChildDocument(resolver, rootUri, documentUri)
        }

        private fun readRequest(input: InputStream): Request {
            val bytes = ArrayList<Byte>()
            var state = 0
            while (bytes.size < MAX_HEADER_BYTES) {
                val value = input.read()
                if (value < 0) throw EOFException()
                bytes += value.toByte()
                state = when {
                    state == 0 && value == '\r'.code -> 1
                    state == 1 && value == '\n'.code -> 2
                    state == 2 && value == '\r'.code -> 3
                    state == 3 && value == '\n'.code -> 4
                    else -> 0
                }
                if (state == 4) break
            }
            require(state == 4)
            val text = bytes.toByteArray().toString(StandardCharsets.ISO_8859_1)
            val lines = text.removeSuffix("\r\n\r\n").split("\r\n")
            val first = lines.first().split(' ')
            require(first.size == 3 && first[2] == "HTTP/1.1")
            val method = first[0].uppercase(Locale.ROOT)
            require(method.all { it in 'A'..'Z' })
            val headers = linkedMapOf<String, String>()
            lines.drop(1).forEach { line ->
                val index = line.indexOf(':')
                require(index in 1..256)
                val name = line.substring(0, index).lowercase(Locale.ROOT)
                require(name.all { it in 'a'..'z' || it == '-' })
                require(headers.put(name, line.substring(index + 1).trim()) == null)
            }
            return Request(method, first[1], headers)
        }

        private fun parsePath(target: String): List<String> {
            val rawPath = if (target.startsWith("/")) target.substringBefore('?') else URI(target).rawPath
            require(rawPath.startsWith('/') && !target.contains('?'))
            if (rawPath == "/") return emptyList()
            require(!rawPath.endsWith('/'))
            return rawPath.removePrefix("/").split('/').map(::decodeComponent)
        }

        private fun decodeComponent(value: String): String {
            require(value.isNotEmpty())
            val bytes = ByteArray(value.length * 4)
            var size = 0
            var index = 0
            while (index < value.length) {
                if (value[index] == '%') {
                    require(index + 2 < value.length)
                    val high = value[index + 1].digitToIntOrNull(16) ?: throw IllegalArgumentException()
                    val low = value[index + 2].digitToIntOrNull(16) ?: throw IllegalArgumentException()
                    bytes[size++] = ((high shl 4) or low).toByte()
                    index += 3
                } else {
                    val encoded = value[index].toString().encodeToByteArray()
                    encoded.copyInto(bytes, size)
                    size += encoded.size
                    index += 1
                }
            }
            val decoded = try {
                StandardCharsets.UTF_8.newDecoder()
                    .onMalformedInput(CodingErrorAction.REPORT)
                    .onUnmappableCharacter(CodingErrorAction.REPORT)
                    .decode(java.nio.ByteBuffer.wrap(bytes, 0, size)).toString()
            } catch (_: Exception) {
                throw IllegalArgumentException("Invalid path encoding.")
            } finally {
                bytes.fill(0)
            }
            require(
                decoded.isNotEmpty() && decoded != "." && decoded != ".." &&
                    Normalizer.isNormalized(decoded, Normalizer.Form.NFC) &&
                    decoded.none { it == '/' || it == '\\' || it == '\u0000' || it.isISOControl() },
            )
            return decoded
        }

        private fun encodedPath(path: List<String>, directory: Boolean): String {
            val encoded = path.joinToString("/", prefix = "/") { component ->
                buildString {
                    component.encodeToByteArray().forEach { byte ->
                        val value = byte.toInt() and 0xff
                        if ((value in 'a'.code..'z'.code) || (value in 'A'.code..'Z'.code) ||
                            (value in '0'.code..'9'.code) || value in listOf('-'.code, '_'.code, '.'.code, '~'.code)
                        ) append(value.toChar()) else append("%%%02X".format(Locale.ROOT, value))
                    }
                }
            }
            return if (directory && encoded != "/") "$encoded/" else encoded
        }

        private fun copyExactly(input: InputStream, output: OutputStream, length: Long) {
            var remaining = length
            val buffer = ByteArray(BUFFER_BYTES)
            while (remaining > 0) {
                if (Thread.currentThread().isInterrupted) throw InterruptedException()
                val count = input.read(buffer, 0, min(buffer.size.toLong(), remaining).toInt())
                if (count < 0) throw EOFException()
                output.write(buffer, 0, count)
                remaining -= count
            }
        }

        private fun writeResponse(
            output: OutputStream,
            code: Int,
            reason: String,
            body: ByteArray = ByteArray(0),
            headers: Map<String, String> = emptyMap(),
        ) {
            writeHead(output, code, reason, headers + mapOf("Content-Length" to body.size.toString()))
            output.write(body)
            output.flush()
        }

        private fun writeHead(output: OutputStream, code: Int, reason: String, headers: Map<String, String>) {
            val value = buildString {
                append("HTTP/1.1 $code $reason\r\n")
                append("Connection: close\r\n")
                append("Cache-Control: no-store\r\n")
                headers.forEach { (name, content) -> append(name).append(": ").append(content).append("\r\n") }
                append("\r\n")
            }
            output.write(value.toByteArray(StandardCharsets.ISO_8859_1))
            output.flush()
        }

        private fun xmlEscape(value: String): String = buildString(value.length) {
            value.forEach {
                append(when (it) {
                    '&' -> "&amp;"
                    '<' -> "&lt;"
                    '>' -> "&gt;"
                    '"' -> "&quot;"
                    '\'' -> "&apos;"
                    else -> it
                })
            }
        }

        private fun validMimeType(value: String): Boolean =
            value.length in 1..512 && value.none { it.isISOControl() || it == ';' }

        private fun httpDate(epochMillis: Long): String = HTTP_DATE.format(Instant.ofEpochMilli(epochMillis))
    }
}
