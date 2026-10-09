package life.michaelwong.covalent.data

import android.content.Context
import android.net.Uri
import android.provider.DocumentsContract
import java.io.Closeable
import java.io.IOException
import java.io.InputStream
import java.io.InterruptedIOException
import java.security.MessageDigest
import java.util.Base64

internal const val MAX_RECOVERY_KIT_BYTES = 16 * 1_024 * 1_024
internal const val MAX_RECOVERY_CODE_FILE_BYTES = 512
private const val RECOVERY_CODE_LENGTH = 43

internal class RecoveryFileException(message: String, cause: Throwable? = null) : IOException(message, cause)

/**
 * One API-generated kit/code pair. It is never serializable and owns every mutable copy.
 * A successful code write clears the code immediately; a failed kit write can then be retried
 * without asking the node to generate a different pair.
 */
internal class RecoveryExportMaterial(
    private val kit: ByteArray,
    private val code: ByteArray,
) : Closeable {
    var codeSaved: Boolean = false
        private set
    var kitSaved: Boolean = false
        private set
    private var closed = false

    init {
        require(kit.size in 1..MAX_RECOVERY_KIT_BYTES)
        validateRecoveryCodeBytes(code)
    }

    @Synchronized
    fun save(context: Context, kitDestination: Uri, codeDestination: Uri) {
        requireDistinctRecoveryDocuments(kitDestination, codeDestination)
        saveWith(
            codeWriter = { writeAndVerify(context, codeDestination, it, "recovery code") },
            kitWriter = { writeAndVerify(context, kitDestination, it, "recovery kit") },
        )
    }

    @Synchronized
    internal fun saveWith(
        codeWriter: (ByteArray) -> Unit,
        kitWriter: (ByteArray) -> Unit,
    ) {
        check(!closed) { "This recovery export is no longer available." }
        if (!codeSaved) {
            codeWriter(code)
            codeSaved = true
            code.fill(0)
        }
        if (!kitSaved) {
            kitWriter(kit)
            kitSaved = true
            kit.fill(0)
        }
    }

    val complete: Boolean get() = codeSaved && kitSaved

    override fun close() {
        synchronized(this) {
            if (closed) return
            kit.fill(0)
            code.fill(0)
            closed = true
        }
    }

    override fun toString(): String = "RecoveryExportMaterial([REDACTED])"
}

/** Raw encrypted kit bytes and one decoded 256-bit recovery key, ready for one JNI call. */
internal class RecoveryBootstrapMaterial(
    internal val kit: ByteArray,
    internal val key: ByteArray,
) : Closeable {
    private var closed = false

    init {
        require(kit.size in 1..MAX_RECOVERY_KIT_BYTES)
        require(key.size == 32)
    }

    override fun close() {
        synchronized(this) {
            if (closed) return
            kit.fill(0)
            key.fill(0)
            closed = true
        }
    }

    override fun toString(): String = "RecoveryBootstrapMaterial([REDACTED])"
}

internal object RecoverySafFiles {
    fun readBootstrap(context: Context, kitUri: Uri, codeUri: Uri): RecoveryBootstrapMaterial {
        requireDistinctRecoveryDocuments(kitUri, codeUri)
        val kit = readDocument(context, kitUri, MAX_RECOVERY_KIT_BYTES, "recovery kit")
        var key: ByteArray? = null
        return try {
            require(kit.isNotEmpty()) { "The recovery kit is empty." }
            val code = readDocument(context, codeUri, MAX_RECOVERY_CODE_FILE_BYTES, "recovery code")
            try {
                key = decodeRecoveryCode(code)
            } finally {
                code.fill(0)
            }
            RecoveryBootstrapMaterial(kit, checkNotNull(key))
        } catch (error: Throwable) {
            kit.fill(0)
            key?.fill(0)
            throw error
        }
    }
}

internal fun decodeRecoveryCode(input: ByteArray): ByteArray {
    var start = 0
    var end = input.size
    while (start < end && input[start].isAsciiWhitespace()) start += 1
    while (end > start && input[end - 1].isAsciiWhitespace()) end -= 1
    require(end - start == RECOVERY_CODE_LENGTH) {
        "The recovery code must be the 43-character code saved by Covalent."
    }
    val encoded = input.copyOfRange(start, end)
    return try {
        validateRecoveryCodeBytes(encoded)
        Base64.getUrlDecoder().decode(encoded).also {
            require(it.size == 32) { "The recovery code is invalid." }
        }
    } catch (error: IllegalArgumentException) {
        throw IllegalArgumentException("The recovery code is invalid.", error)
    } finally {
        encoded.fill(0)
    }
}

private fun validateRecoveryCodeBytes(code: ByteArray) {
    require(code.size == RECOVERY_CODE_LENGTH && code.all { byte ->
        val character = byte.toInt().toChar()
        (character.isLetterOrDigit() && character.code < 128) || character == '_' || character == '-'
    }) { "The recovery code is invalid." }
}

private fun Byte.isAsciiWhitespace(): Boolean = when (toInt().and(0xff)) {
    9, 10, 13, 32 -> true
    else -> false
}

private fun requireDistinctRecoveryDocuments(first: Uri, second: Uri) {
    val sameDocument = first == second || runCatching {
        first.scheme == second.scheme && first.authority == second.authority &&
            DocumentsContract.getDocumentId(first) == DocumentsContract.getDocumentId(second)
    }.getOrDefault(false)
    require(!sameDocument) { "Choose different files for the recovery kit and recovery code." }
}

private fun readDocument(context: Context, uri: Uri, maximum: Int, label: String): ByteArray {
    val stream = context.contentResolver.openInputStream(uri)
        ?: throw RecoveryFileException("The selected $label could not be opened.")
    return try {
        stream.use { it.readBounded(maximum, label) }
    } catch (error: InterruptedIOException) {
        throw error
    } catch (error: RecoveryFileException) {
        throw error
    } catch (error: IOException) {
        throw RecoveryFileException("The selected $label could not be read.", error)
    }
}

internal fun InputStream.readBounded(maximum: Int, label: String): ByteArray {
    require(maximum >= 0)
    var collected = ByteArray(minOf(maximum, 16 * 1_024).coerceAtLeast(1))
    val buffer = ByteArray(16 * 1_024)
    var total = 0
    var emptyReads = 0
    try {
        while (true) {
            val count = read(buffer)
            if (count < 0) break
            if (count == 0) {
                emptyReads += 1
                if (emptyReads >= 32) throw RecoveryFileException("$label stopped making progress.")
                continue
            }
            emptyReads = 0
            total = try {
                Math.addExact(total, count)
            } catch (_: ArithmeticException) {
                throw RecoveryFileException("$label is too large.")
            }
            if (total > maximum) throw RecoveryFileException("$label is too large.")
            if (total > collected.size) {
                val grownSize = maxOf(total, collected.size.saturatingDouble()).coerceAtMost(maximum)
                val grown = ByteArray(grownSize)
                collected.copyInto(grown, endIndex = total - count)
                collected.fill(0)
                collected = grown
            }
            buffer.copyInto(collected, destinationOffset = total - count, endIndex = count)
        }
        return collected.copyOf(total).also { collected.fill(0) }
    } catch (error: Throwable) {
        collected.fill(0)
        throw error
    } finally {
        buffer.fill(0)
    }
}

internal fun InputStream.readBoundedText(maximum: Int, label: String): String {
    val bytes = readBounded(maximum, label)
    return try {
        bytes.decodeToString()
    } finally {
        bytes.fill(0)
    }
}

private fun Int.saturatingDouble(): Int = if (this > Int.MAX_VALUE / 2) Int.MAX_VALUE else this * 2

private fun writeAndVerify(context: Context, uri: Uri, bytes: ByteArray, label: String) {
    writeAndVerify(
        bytes = bytes,
        label = label,
        openOutput = { context.contentResolver.openOutputStream(uri, "wt") },
        openInput = { context.contentResolver.openInputStream(uri) },
    )
}

internal fun writeAndVerify(
    bytes: ByteArray,
    label: String,
    openOutput: () -> java.io.OutputStream?,
    openInput: () -> InputStream?,
) {
    try {
        openOutput()?.use { output ->
            output.write(bytes)
            output.flush()
        } ?: throw RecoveryFileException("The selected $label file could not be opened for writing.")

        val expectedDigest = MessageDigest.getInstance("SHA-256").digest(bytes)
        val actualDigest = MessageDigest.getInstance("SHA-256")
        var actualLength = 0L
        var emptyReads = 0
        val buffer = ByteArray(16 * 1_024)
        try {
            openInput()?.use { input ->
                while (true) {
                    val count = input.read(buffer)
                    if (count < 0) break
                    if (count == 0) {
                        emptyReads += 1
                        if (emptyReads >= 32) {
                            throw RecoveryFileException("The selected $label file could not be verified.")
                        }
                        continue
                    }
                    emptyReads = 0
                    actualLength = Math.addExact(actualLength, count.toLong())
                    if (actualLength > bytes.size.toLong()) {
                        throw RecoveryFileException("The selected $label file did not preserve the saved data.")
                    }
                    actualDigest.update(buffer, 0, count)
                }
            } ?: throw RecoveryFileException("The selected $label file could not be verified.")
            val actualDigestBytes = actualDigest.digest()
            try {
                if (actualLength != bytes.size.toLong() || !MessageDigest.isEqual(expectedDigest, actualDigestBytes)) {
                    throw RecoveryFileException("The selected $label file did not preserve the saved data.")
                }
            } finally {
                actualDigestBytes.fill(0)
            }
        } finally {
            expectedDigest.fill(0)
            buffer.fill(0)
        }
    } catch (error: InterruptedIOException) {
        throw error
    } catch (error: RecoveryFileException) {
        throw error
    } catch (error: IOException) {
        throw RecoveryFileException("The selected $label file could not be saved.", error)
    }
}
