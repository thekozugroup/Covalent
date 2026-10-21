package life.michaelwong.covalent.data

import java.io.IOException
import java.net.SocketTimeoutException
import org.junit.Assert.assertEquals
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test

class FolderAcceptanceRetryTest {
    private fun busy(status: Int = 503, version: Int = 1, code: String = "folder_sync_busy", retryable: Boolean = true) =
        NodeApiException(status, version, code, retryable, "Busy", hasExplicitRetryContract = true)

    @Test
    fun immediateSuccessDoesNotWait() {
        assertEquals("accepted", retryFolderAcceptance(wait = { error("Unexpected wait") }) { "accepted" })
    }

    @Test
    fun transientBusyCompletesOnceAfterBoundedBackoff() {
        val waits = mutableListOf<Long>()
        var calls = 0
        val accepted = Any()
        val result = retryFolderAcceptance(wait = waits::add) {
            calls++
            if (calls < 3) throw busy()
            accepted
        }
        assertSame(accepted, result)
        assertEquals(3, calls)
        assertEquals(listOf(100L, 200L), waits)
    }

    @Test
    fun persistentBusyReturnsTheLastRefusalAfterSixAttempts() {
        val waits = mutableListOf<Long>()
        var calls = 0
        val refusal = busy()
        val result = runCatching { retryFolderAcceptance(wait = waits::add) { calls++; throw refusal } }
        assertSame(refusal, result.exceptionOrNull())
        assertEquals(6, calls)
        assertEquals(listOf(100L, 200L, 400L, 800L, 1_600L), waits)
    }

    @Test
    fun otherErrorsAreNotRetried() {
        val failures = listOf(busy(status = 500), busy(status = 401), busy(status = 409),
            busy(version = 2), busy(code = "folder_sync_needs_attention"), busy(retryable = false))
        failures.forEach { failure ->
            var calls = 0
            val result = runCatching { retryFolderAcceptance(wait = { error("Unexpected wait") }) { calls++; throw failure } }
            assertSame(failure, result.exceptionOrNull())
            assertEquals(1, calls)
        }
    }

    @Test
    fun inferredErrorDefaultsNeverAuthorizeReplay() {
        val failure = NodeApiException(503, 1, "folder_sync_busy", true, "Busy")
        var calls = 0
        val result = runCatching {
            retryFolderAcceptance(wait = { error("Unexpected wait") }) { calls++; throw failure }
        }
        assertSame(failure, result.exceptionOrNull())
        assertEquals(1, calls)
    }

    @Test
    fun uncertainTransportAndInvalidResponsesAreNeverReplayed() {
        listOf(IOException("Lost response"), SocketTimeoutException("No response"),
            NodeProtocolException(2), IllegalStateException("Invalid acknowledgement")).forEach { failure ->
            var calls = 0
            val result = runCatching { retryFolderAcceptance(wait = { error("Unexpected wait") }) { calls++; throw failure } }
            assertSame(failure, result.exceptionOrNull())
            assertEquals(1, calls)
        }
    }

    @Test
    fun interruptionDuringBackoffPreservesCancellationAndSendsNothingElse() {
        var calls = 0
        val cancelled = InterruptedException("Cancelled")
        try {
            val result = runCatching {
                retryFolderAcceptance(wait = { throw cancelled }) { calls++; throw busy() }
            }
            assertSame(cancelled, result.exceptionOrNull())
            assertEquals(1, calls)
            assertTrue(Thread.currentThread().isInterrupted)
        } finally { Thread.interrupted() }
    }

    @Test
    fun alreadyInterruptedCallerNeverSendsAcceptance() {
        var calls = 0
        try {
            Thread.currentThread().interrupt()
            val result = runCatching { retryFolderAcceptance(wait = { error("Unexpected wait") }) { calls++; "accepted" } }
            assertTrue(result.exceptionOrNull() is InterruptedException)
            assertTrue(Thread.currentThread().isInterrupted)
            assertEquals(0, calls)
        } finally { Thread.interrupted() }
    }
}
