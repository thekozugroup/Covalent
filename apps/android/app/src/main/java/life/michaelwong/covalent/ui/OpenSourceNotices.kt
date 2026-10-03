package life.michaelwong.covalent.ui

import android.content.Context
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.produceState
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import java.io.InputStream
import java.nio.ByteBuffer
import java.nio.charset.CodingErrorAction
import java.nio.charset.StandardCharsets
import java.security.MessageDigest
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import life.michaelwong.covalent.R

private const val INDEX_ASSET = "sync-engine-notices-index.txt"
private const val COMBINED_ASSET = "sync-engine-notices/THIRD-PARTY-NOTICES.txt"
private const val MANIFEST_ASSET = "sync-engine-notices/manifest.json"
private const val MAXIMUM_INDEX_BYTES = 1_024
private const val MAXIMUM_NOTICE_BYTES = 8 * 1_024 * 1_024
private const val MAXIMUM_MANIFEST_BYTES = 4 * 1_024 * 1_024
private val SHA256_PATTERN = Regex("[0-9a-f]{64}")
private val CANONICAL_SIZE_PATTERN = Regex("[1-9][0-9]*")

internal data class NoticeAssetDescriptor(
    val path: String,
    val sha256: String,
    val bytes: Int,
)

internal data class PackagedNoticeIndex(
    val combined: NoticeAssetDescriptor,
    val manifest: NoticeAssetDescriptor,
)

internal fun interface NoticeAssetSource {
    fun open(path: String): InputStream
}

/**
 * Reads only the build-generated, package-owned notice assets. Both the human-readable
 * notice and its provenance manifest must match the compact package descriptor before
 * any text is shown. This verifies packaging integrity; it is not a legal approval.
 */
internal object OpenSourceNotices {
    fun load(context: Context): String = load(
        NoticeAssetSource { path -> context.assets.open(path) },
    )

    fun load(source: NoticeAssetSource): String {
        val indexBytes = source.open(INDEX_ASSET).use {
            readAtMost(it, MAXIMUM_INDEX_BYTES)
        }
        val index = parseIndex(strictDecode(indexBytes, StandardCharsets.US_ASCII))
        val combined = readVerified(source, index.combined, MAXIMUM_NOTICE_BYTES)
        // The manifest is retained for distribution provenance. Verify it even though
        // the native viewer renders the complete combined notice text instead.
        readVerified(source, index.manifest, MAXIMUM_MANIFEST_BYTES)
        return strictDecode(combined, StandardCharsets.UTF_8)
    }

    internal fun parseIndex(value: String): PackagedNoticeIndex {
        check(!value.contains('\r') && value.endsWith('\n')) { "Invalid notice index" }
        val lines = value.dropLast(1).split('\n')
        check(lines.size == 3 && lines[0] == "1") { "Invalid notice index" }

        fun parseLine(line: String, expectedLabel: String, expectedPath: String): NoticeAssetDescriptor {
            val fields = line.split(' ')
            check(fields.size == 4 && fields[0] == expectedLabel && fields[3] == expectedPath) {
                "Invalid notice index"
            }
            check(SHA256_PATTERN.matches(fields[1])) { "Invalid notice index" }
            check(CANONICAL_SIZE_PATTERN.matches(fields[2])) { "Invalid notice index" }
            val bytes = fields[2].toLongOrNull()
            val maximum = if (expectedLabel == "combined") {
                MAXIMUM_NOTICE_BYTES
            } else {
                MAXIMUM_MANIFEST_BYTES
            }
            check(bytes != null && bytes <= maximum) { "Invalid notice index" }
            return NoticeAssetDescriptor(expectedPath, fields[1], bytes.toInt())
        }

        return PackagedNoticeIndex(
            combined = parseLine(lines[1], "combined", COMBINED_ASSET),
            manifest = parseLine(lines[2], "manifest", MANIFEST_ASSET),
        )
    }

    private fun readVerified(
        source: NoticeAssetSource,
        descriptor: NoticeAssetDescriptor,
        maximumBytes: Int,
    ): ByteArray {
        check(descriptor.bytes in 1..maximumBytes) { "Invalid notice descriptor" }
        val bytes = source.open(descriptor.path).use { readExactly(it, descriptor.bytes) }
        val digest = MessageDigest.getInstance("SHA-256").digest(bytes).toHex()
        check(digest == descriptor.sha256) { "Packaged notice digest mismatch" }
        return bytes
    }

    private fun readAtMost(input: InputStream, maximumBytes: Int): ByteArray {
        val result = java.io.ByteArrayOutputStream(minOf(maximumBytes, 4 * 1_024))
        val buffer = ByteArray(1_024)
        var total = 0
        while (true) {
            val count = input.read(buffer)
            if (count < 0) break
            check(count > 0) { "Packaged notice asset made no read progress" }
            total += count
            check(total <= maximumBytes) { "Packaged notice asset exceeds its bound" }
            result.write(buffer, 0, count)
        }
        return result.toByteArray()
    }

    private fun readExactly(input: InputStream, expectedBytes: Int): ByteArray {
        val result = ByteArray(expectedBytes)
        var offset = 0
        while (offset < result.size) {
            val count = input.read(result, offset, result.size - offset)
            if (count < 0) error("Packaged notice asset is truncated")
            if (count == 0) {
                val next = input.read()
                if (next < 0) error("Packaged notice asset is truncated")
                result[offset++] = next.toByte()
            } else {
                offset += count
            }
        }
        check(input.read() < 0) { "Packaged notice asset has trailing bytes" }
        return result
    }

    private fun strictDecode(bytes: ByteArray, charset: java.nio.charset.Charset): String =
        charset.newDecoder()
            .onMalformedInput(CodingErrorAction.REPORT)
            .onUnmappableCharacter(CodingErrorAction.REPORT)
            .decode(ByteBuffer.wrap(bytes))
            .toString()

    private fun ByteArray.toHex(): String = joinToString("") { byte -> "%02x".format(byte) }
}

private sealed interface NoticeViewerState {
    data object Loading : NoticeViewerState
    data class Ready(val text: String) : NoticeViewerState
    data object Unavailable : NoticeViewerState
}

@Composable
internal fun OpenSourceNoticesDialog(onDismiss: () -> Unit) {
    val context = LocalContext.current
    val state by produceState<NoticeViewerState>(NoticeViewerState.Loading, context) {
        value = withContext(Dispatchers.IO) {
            runCatching { OpenSourceNotices.load(context) }
                .fold(NoticeViewerState::Ready) { NoticeViewerState.Unavailable }
        }
    }
    AlertDialog(
        modifier = Modifier.testTag("settings.openSourceNotices.dialog"),
        onDismissRequest = onDismiss,
        title = { Text(stringResource(R.string.open_source_notices_title)) },
        text = {
            Box(
                Modifier
                    .fillMaxWidth()
                    .heightIn(min = 160.dp, max = 520.dp)
                    .verticalScroll(rememberScrollState())
                    .padding(vertical = 4.dp),
                contentAlignment = Alignment.Center,
            ) {
                when (val current = state) {
                    NoticeViewerState.Loading -> CircularProgressIndicator()
                    NoticeViewerState.Unavailable -> Text(
                        stringResource(R.string.open_source_notices_error),
                        color = MaterialTheme.colorScheme.error,
                    )
                    is NoticeViewerState.Ready -> Text(
                        current.text,
                        fontFamily = FontFamily.Monospace,
                        style = MaterialTheme.typography.bodySmall,
                        modifier = Modifier.fillMaxWidth(),
                    )
                }
            }
        },
        confirmButton = {
            TextButton(onClick = onDismiss) {
                Text(stringResource(R.string.action_close))
            }
        },
    )
}
