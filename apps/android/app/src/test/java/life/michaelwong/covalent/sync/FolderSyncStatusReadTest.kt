package life.michaelwong.covalent.sync

import java.net.ConnectException
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.runBlocking
import life.michaelwong.covalent.data.NodeApiException
import life.michaelwong.covalent.model.*
import org.junit.Assert.*
import org.junit.Test

class FolderSyncStatusReadTest {
    private val ready = FolderSyncStatus(
        FolderSyncAvailability.AVAILABLE, FolderSyncLifecycle.RUNNING, null,
        FolderHealthFreshness.FRESH, emptyList(), emptyList(), emptyList(),
    )

    @Test fun permissionRestartWaitsForTheAuthenticatedStatusWithoutChangingIt() = runBlocking {
        var reads = 0
        var pauses = 0
        val result = readFolderSyncStatusWhenReady(pause = { pauses++ }) {
            when (++reads) {
                1 -> throw ConnectException()
                2 -> throw NodeApiException(503, 1, "starting", true, "Starting")
                else -> ready
            }
        }
        assertSame(ready, result)
        assertEquals(3, reads)
        assertEquals(2, pauses)
    }

    @Test fun persistentTransportFailureIsBoundedAndPreserved() {
        val failure = ConnectException()
        var reads = 0
        val thrown = assertThrows(ConnectException::class.java) {
            runBlocking {
                readFolderSyncStatusWhenReady(attempts = 3, pause = {}) { reads++; throw failure }
            }
        }
        assertSame(failure, thrown)
        assertEquals(3, reads)
    }

    @Test fun authenticationProtocolAndCancellationFailuresAreNeverRetried() {
        for (failure in listOf(
            NodeApiException(401, 1, "unauthorized", false, "Unauthorized"),
            IllegalStateException("Invalid status"), CancellationException(),
        )) {
            var reads = 0
            val thrown = assertThrows(failure.javaClass) {
                runBlocking {
                    readFolderSyncStatusWhenReady(pause = { fail("Must not wait") }) {
                        reads++; throw failure
                    }
                }
            }
            assertSame(failure, thrown)
            assertEquals(1, reads)
        }
    }
}
