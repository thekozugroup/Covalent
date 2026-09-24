package life.michaelwong.covalent

import java.io.IOException
import java.io.InputStream
import java.io.InterruptedIOException
import java.io.OutputStream
import java.util.concurrent.ConcurrentHashMap
import life.michaelwong.covalent.data.SafResourceLimitException
import life.michaelwong.covalent.data.SafSourceAccessException
import life.michaelwong.covalent.data.SafSourceInputStream
import life.michaelwong.covalent.data.SafTargetAccessException
import life.michaelwong.covalent.data.SafTargetOutputStream
import life.michaelwong.covalent.data.archiveUploadRestartOffset
import life.michaelwong.covalent.data.requireSafEntryCapacity
import life.michaelwong.covalent.ui.retainFolderSelection
import life.michaelwong.covalent.work.TransferOutcome
import life.michaelwong.covalent.work.completeIfStillRunning
import life.michaelwong.covalent.work.guardedTransferExecution
import life.michaelwong.covalent.work.safSecurityFailureMessageRes
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class SafFailureAndSchedulerTest {
    @Test
    fun folderReplacementClearsRejectedSelectionButCancellationPreservesIt() {
        val cancelled = retainFolderSelection("folder-a", null) {
            error("must not retain cancellation")
        }
        assertEquals("folder-a", cancelled.selected)
        assertFalse(cancelled.replacementAttempted)

        val failure = SecurityException("permission not persisted")
        val rejected = retainFolderSelection("folder-a", "folder-b") { throw failure }
        assertNull(rejected.selected)
        assertTrue(rejected.replacementAttempted)
        assertSame(failure, rejected.failure)

        val accepted = retainFolderSelection("folder-a", "folder-b") {}
        assertEquals("folder-b", accepted.selected)
        assertTrue(accepted.replacementAttempted)
        assertNull(accepted.failure)
    }

    @Test
    fun stoppedJobSuppressesItsQueuedMainThreadCompletion() {
        val running = ConcurrentHashMap<String, Unit>().apply { put("job", Unit) }
        var completions = 0

        running.remove("job") // onStopJob wins before the posted completion callback.
        completeIfStillRunning(running, "job") { completions += 1 }
        assertEquals(0, completions)

        running["job"] = Unit
        completeIfStillRunning(running, "job") { completions += 1 }
        completeIfStillRunning(running, "job") { completions += 1 }
        assertEquals(1, completions)
    }

    @Test
    fun schedulerInterruptionNeverStartsAnotherArchiveRequest() {
        val interruption = InterruptedIOException("stopped")
        var nextRequestStarted = false

        val shown = assertThrows(InterruptedIOException::class.java) {
            archiveUploadRestartOffset(interruption, alreadyRestarted = false)
            nextRequestStarted = true
        }

        assertSame(interruption, shown)
        assertFalse(nextRequestStarted)
    }

    @Test
    fun providerCursorStopsBeforeGrowingPastItsTransferLimit() {
        requireSafEntryCapacity(currentCount = 99_999, maximumEntries = 100_000)
        assertThrows(SafResourceLimitException::class.java) {
            requireSafEntryCapacity(currentCount = 100_000, maximumEntries = 100_000)
        }
    }

    @Test
    fun sourceProviderFailureIsNotMistakenForNetworkFailure() {
        val failure = IOException("provider read failed")
        val stream = SafSourceInputStream(object : InputStream() {
            override fun read(): Int = throw failure
        })

        val shown = assertThrows(SafSourceAccessException::class.java) { stream.read() }
        assertSame(failure, shown.cause)
    }

    @Test
    fun restoreProviderFailureIsNotMistakenForNetworkFailure() {
        val failure = IOException("provider write failed")
        val stream = SafTargetOutputStream(object : OutputStream() {
            override fun write(value: Int) = throw failure
        })

        val shown = assertThrows(SafTargetAccessException::class.java) { stream.write(1) }
        assertSame(failure, shown.cause)
    }

    @Test
    fun providerStreamsPreserveSchedulerInterruption() {
        val interruption = InterruptedIOException("stopped")
        val stream = SafSourceInputStream(object : InputStream() {
            override fun read(): Int = throw interruption
        })

        assertSame(interruption, assertThrows(InterruptedIOException::class.java) { stream.read() })
    }

    @Test
    fun unexpectedExecutionStillCompletesTheSchedulerAttemptForRetry() {
        val failure = IllegalStateException("failure before guarded transfer body")
        var recorded: Exception? = null

        val outcome = guardedTransferExecution(
            execute = { throw failure },
            onUnexpected = { recorded = it },
        )

        assertEquals(TransferOutcome.RETRY, outcome)
        assertSame(failure, recorded)
    }

    @Test
    fun schedulerRecoveryFailureStillReturnsARetryOutcome() {
        assertEquals(
            TransferOutcome.RETRY,
            guardedTransferExecution(
                execute = { error("execution failed") },
                onUnexpected = { error("recovery persistence failed") },
            ),
        )
    }

    @Test
    fun revokedPermissionNamesTheFolderUsedByTheTransfer() {
        assertEquals(
            R.string.error_source_access_revoked,
            safSecurityFailureMessageRes("saf_backup"),
        )
        assertEquals(
            R.string.error_target_access_revoked,
            safSecurityFailureMessageRes("saf_restore"),
        )
        assertEquals(R.string.error_node_action_failed, safSecurityFailureMessageRes("json"))
    }
}
