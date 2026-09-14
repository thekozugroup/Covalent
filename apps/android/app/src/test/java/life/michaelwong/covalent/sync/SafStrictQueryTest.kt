package life.michaelwong.covalent.sync

import android.provider.DocumentsContract
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class SafStrictQueryTest {
    @Test
    fun stableMetadataKeepsOpaqueIdsAndDoesNotClaimContentProof() {
        val source = FakeSource(
            children = mapOf(
                ROOT to listOf(
                    directory("opaque:/directory", "Folder"),
                    file("opaque:/file?token=kept", "Report.txt", 12),
                ),
                "opaque:/directory" to listOf(file("opaque:/nested#1", "nested.bin", 3)),
            ),
        )

        val observation = SafSyncInventory(source, SafSyncInventoryLimits()).observe(ROOT)

        assertEquals(
            listOf("opaque:/directory", "opaque:/file?token=kept", "opaque:/nested#1"),
            observation.entries.map { it.documentId },
        )
        assertEquals(ROOT, observation.rootDocumentId)
        assertEquals(0, source.contentOpenCount)
        assertTrue(observation.toString().contains("contentVerified=false"))
        assertFalse(observation.entries.first().toString().contains("Folder"))
    }

    @Test
    fun mutationBetweenFullScansRejectsTheWholeObservation() {
        val source = FakeSource(
            children = mapOf(ROOT to listOf(file("file-a", "a.txt", 1))),
            secondScanChildren = mapOf(ROOT to listOf(file("file-b", "b.txt", 1))),
        )

        assertReason(SafSyncInventoryFailure.UNSTABLE_OBSERVATION) {
            SafSyncInventory(source, SafSyncInventoryLimits()).observe(ROOT)
        }
    }

    @Test
    fun providerFailureAfterRowsDoesNotReturnAPartialInventory() {
        val source = FakeSource(
            children = mapOf(ROOT to listOf(file("file-a", "a.txt", 1))),
            failAfterFirstChild = true,
        )

        assertReason(SafSyncInventoryFailure.PROVIDER_QUERY_FAILED) {
            SafSyncInventory(source, SafSyncInventoryLimits()).observe(ROOT)
        }
    }

    @Test
    fun cyclesAndRepeatedOpaqueIdsAreVisibleFailures() {
        assertReason(SafSyncInventoryFailure.DOCUMENT_CYCLE) {
            SafSyncInventory(
                FakeSource(children = mapOf(ROOT to listOf(directory(ROOT, "again")))),
                SafSyncInventoryLimits(),
            ).observe(ROOT)
        }
        assertReason(SafSyncInventoryFailure.DUPLICATE_DOCUMENT_ID) {
            SafSyncInventory(
                FakeSource(
                    children = mapOf(
                        ROOT to listOf(directory("left", "left"), directory("right", "right")),
                        "left" to listOf(file("shared", "one.txt", 1)),
                        "right" to listOf(file("shared", "two.txt", 1)),
                    ),
                ),
                SafSyncInventoryLimits(),
            ).observe(ROOT)
        }
    }

    @Test
    fun partialAndVirtualDocumentsFailClosed() {
        assertReason(SafSyncInventoryFailure.PARTIAL_DOCUMENT) {
            observeSingle(file("partial", "partial.txt", 1, DocumentsContract.Document.FLAG_PARTIAL))
        }
        assertReason(SafSyncInventoryFailure.VIRTUAL_DOCUMENT) {
            observeSingle(file("virtual", "virtual.txt", 1, DocumentsContract.Document.FLAG_VIRTUAL_DOCUMENT))
        }
    }

    @Test
    fun everyTraversalResourceLimitIsEnforcedBeforeAResult() {
        assertReason(SafSyncInventoryFailure.ENTRY_LIMIT) {
            observeSingle(file("one", "one.txt", 1), SafSyncInventoryLimits(maxEntries = 1)) {
                mapOf(ROOT to listOf(file("one", "one.txt", 1), file("two", "two.txt", 1)))
            }
        }
        assertReason(SafSyncInventoryFailure.DIRECTORY_LIMIT) {
            observeSingle(directory("dir", "dir"), SafSyncInventoryLimits(maxDirectories = 1))
        }
        assertReason(SafSyncInventoryFailure.DEPTH_LIMIT) {
            observeSingle(file("one", "one.txt", 1), SafSyncInventoryLimits(maxDepth = 0))
        }
        assertReason(SafSyncInventoryFailure.NAME_LIMIT) {
            observeSingle(file("one", "12345", 1), SafSyncInventoryLimits(maxNameUtf8Bytes = 4))
        }
        assertReason(SafSyncInventoryFailure.NAME_LIMIT) {
            observeSingle(
                file("one", "four", 1),
                SafSyncInventoryLimits(maxTotalNameUtf8Bytes = 3),
            )
        }
        assertReason(SafSyncInventoryFailure.DOCUMENT_ID_LIMIT) {
            observeSingle(
                file("12345", "one", 1),
                SafSyncInventoryLimits(maxDocumentIdUtf8Bytes = 4),
            )
        }
        assertReason(SafSyncInventoryFailure.MIME_TYPE_LIMIT) {
            observeSingle(file("one", "one", 1).copy(mimeType = "x".repeat(513)))
        }
        assertReason(SafSyncInventoryFailure.METADATA_LIMIT) {
            observeSingle(
                file("one", "one", 1),
                SafSyncInventoryLimits(maxTotalMetadataUtf8Bytes = 60),
            )
        }
    }

    @Test
    fun nestedPendingSiblingsCannotEscapeTheGlobalEntryReservation() {
        val source = FakeSource(
            children = mapOf(
                ROOT to listOf(directory("dir-a", "dir-a"), file("file-b", "file-b", 1)),
                "dir-a" to listOf(file("file-c", "file-c", 1), file("file-d", "file-d", 1)),
            ),
        )

        assertReason(SafSyncInventoryFailure.ENTRY_LIMIT) {
            SafSyncInventory(source, SafSyncInventoryLimits(maxEntries = 3)).observe(ROOT)
        }
    }

    @Test
    fun preliminaryPortableChecksAreConservativeAndVisible() {
        listOf(".", "bad/name", "bad\\name", "CON.txt", "trail. ", "e\u0301.txt").forEachIndexed { index, name ->
            assertReason(SafSyncInventoryFailure.NON_PORTABLE_NAME) {
                observeSingle(file("bad-$index", name, 1))
            }
        }
        assertReason(SafSyncInventoryFailure.PORTABLE_NAME_COLLISION) {
            SafSyncInventory(
                FakeSource(
                    children = mapOf(
                        ROOT to listOf(
                            file("one", "Report.txt", 1),
                            file("two", "report.TXT", 1),
                        ),
                    ),
                ),
                SafSyncInventoryLimits(),
            ).observe(ROOT)
        }
    }

    @Test
    fun cancellationIsCheckedBeforeAndDuringEveryEntry() {
        val source = FakeSource(
            children = mapOf(ROOT to (1..20).map { file("file-$it", "file-$it", 1) }),
            cancelAtCheck = 7,
        )

        assertReason(SafSyncInventoryFailure.CANCELLED) {
            SafSyncInventory(source, SafSyncInventoryLimits()).observe(ROOT)
        }
        assertEquals(0, source.contentOpenCount)
        assertEquals(7, source.cancellationChecks)
    }

    private fun observeSingle(
        child: SafRawMetadata,
        limits: SafSyncInventoryLimits = SafSyncInventoryLimits(),
        childrenOverride: (() -> Map<String, List<SafRawMetadata>>)? = null,
    ): SafSyncMetadataObservation = SafSyncInventory(
        FakeSource(children = childrenOverride?.invoke() ?: mapOf(ROOT to listOf(child))),
        limits,
    ).observe(ROOT)

    private fun assertReason(reason: SafSyncInventoryFailure, action: () -> Unit) {
        val failure = assertThrows(SafSyncInventoryException::class.java) { action() }
        assertEquals(reason, failure.reason)
    }

    private class FakeSource(
        private val children: Map<String, List<SafRawMetadata>>,
        private val secondScanChildren: Map<String, List<SafRawMetadata>>? = null,
        private val failAfterFirstChild: Boolean = false,
        private val cancelAtCheck: Int? = null,
    ) : SafMetadataSource {
        var cancellationChecks = 0
        var contentOpenCount = 0
        private var scan = 0

        override fun throwIfCancelled() {
            cancellationChecks += 1
            if (cancellationChecks == cancelAtCheck) {
                throw SafSyncInventoryException(SafSyncInventoryFailure.CANCELLED)
            }
        }

        override fun queryDocument(documentId: String): SafRawMetadata {
            if (documentId != ROOT) {
                throw SafSyncInventoryException(SafSyncInventoryFailure.PROVIDER_QUERY_FAILED)
            }
            scan += 1
            return directory(ROOT, "Fixture root")
        }

        override fun queryChildren(parentDocumentId: String, consume: (SafRawMetadata) -> Unit) {
            val active = if (scan >= 2 && secondScanChildren != null) secondScanChildren else children
            active[parentDocumentId].orEmpty().forEachIndexed { index, child ->
                consume(child)
                if (failAfterFirstChild && index == 0) {
                    throw SafSyncInventoryException(SafSyncInventoryFailure.PROVIDER_QUERY_FAILED)
                }
            }
        }
    }

    private companion object {
        const val ROOT = "opaque:/root"
        const val DIRECTORY_MIME = "vnd.android.document/directory"

        fun directory(id: String, name: String) = SafRawMetadata(id, name, DIRECTORY_MIME, 0, null, null)

        fun file(id: String, name: String, size: Long, flags: Int = 0) = SafRawMetadata(
            id,
            name,
            "application/octet-stream",
            flags,
            size,
            1_700_000_000_000,
        )
    }
}
