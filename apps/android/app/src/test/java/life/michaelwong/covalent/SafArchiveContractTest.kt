package life.michaelwong.covalent

import life.michaelwong.covalent.data.CovalentNodeClient
import life.michaelwong.covalent.data.SafArchivePath
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Test

class SafArchiveContractTest {
    @Test
    fun archivePathsRejectTraversalAndAmbiguity() {
        listOf("", "/absolute", "../escape", "safe/../escape", "safe//file", "safe\\file").forEach { path ->
            assertThrows(IllegalStateException::class.java) {
                SafArchivePath.parse(path, false)
            }
        }
        assertEquals(
            "Documents/notes.txt",
            SafArchivePath.parse("Documents/notes.txt", false).canonical,
        )
    }

    @Test
    fun authenticatedCleartextIsRestrictedToLoopback() {
        assertThrows(IllegalArgumentException::class.java) {
            CovalentNodeClient().openConnection(
                "http://192.0.2.10:8787",
                "/api/v1/backups",
                "GET",
                "test-local-api-token-with-at-least-32-bytes",
                "application/json",
            )
        }
    }
}
