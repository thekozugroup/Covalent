package life.michaelwong.covalent.data

import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class DurableTransferJournalTest {
    @Test
    fun exactRequestMustCommitBeforeNetworkAndSurvivesStoreRecreation() {
        val storage = FakeDurableStorage()
        val journal = journal(storage)
        storage.failNextCommit = true
        assertThrows(DurableTransferPersistenceException::class.java) {
            journal.saveQueued(
                JOB_ID,
                BACKUP_PATH,
                backupPayload(JOB_ID),
                BACKUP_MODE,
                SOURCE_URI,
                transferRecord(JOB_ID, "queued"),
            )
        }
        assertTrue(storage.disk.isEmpty())

        journal.saveQueued(
            JOB_ID,
            BACKUP_PATH,
            backupPayload(JOB_ID),
            BACKUP_MODE,
            SOURCE_URI,
            transferRecord(JOB_ID, "queued"),
        )
        val recreated = journal(storage)
        val prepared = recreated.preparePending(JOB_ID)
        assertEquals(JOB_ID, prepared?.getString("jobId"))
        assertEquals(JOB_ID, prepared?.getJSONObject("payload")?.getString("jobId"))
        assertEquals(SOURCE_URI, prepared?.getString("treeUri"))
    }

    @Test
    fun failedTerminalCommitLeavesRequestRecoverableAndBlocksAcknowledgement() {
        val storage = FakeDurableStorage()
        val journal = queuedJournal(storage, JOB_ID)
        storage.failNextCommit = true
        assertThrows(DurableTransferPersistenceException::class.java) {
            journal.consumeTerminalResult(JOB_ID, "Complete", transferRecord(JOB_ID, "completed"))
        }

        val recreated = journal(storage)
        assertEquals(JOB_ID, recreated.pending(JOB_ID)?.getString("jobId"))
        assertTrue(recreated.pendingAcknowledgementJobIds().isEmpty())
    }

    @Test
    fun terminalResultAndPendingAcknowledgementCommitAtomicallyAcrossRecreation() {
        val storage = FakeDurableStorage()
        queuedJournal(storage, JOB_ID).consumeTerminalResult(
            JOB_ID,
            "Backup complete",
            transferRecord(JOB_ID, "completed"),
        )

        val recreated = journal(storage)
        assertNull(recreated.pending(JOB_ID))
        assertEquals(listOf(JOB_ID), recreated.pendingAcknowledgementJobIds())
        assertEquals("Backup complete", recreated.acknowledgementCompletionDetail(JOB_ID))
    }

    @Test
    fun acknowledgementMustRecommitBeforeNetworkAndFailureRetainsRecoverableState() {
        val storage = FakeDurableStorage()
        queuedJournal(storage, JOB_ID).consumeTerminalResult(
            JOB_ID,
            "Backup complete",
            transferRecord(JOB_ID, "completed"),
        )
        storage.failNextCommit = true
        assertThrows(DurableTransferPersistenceException::class.java) {
            journal(storage).prepareAcknowledgement(JOB_ID)
        }

        val recreated = journal(storage)
        assertEquals("Backup complete", recreated.acknowledgementCompletionDetail(JOB_ID))
        assertEquals("Backup complete", recreated.prepareAcknowledgement(JOB_ID))
    }

    @Test
    fun serverAckBeforeLocalCleanupIsSafelyRetriedAfterRecreation() {
        val storage = FakeDurableStorage()
        queuedJournal(storage, JOB_ID).consumeTerminalResult(
            JOB_ID,
            "Backup complete",
            transferRecord(JOB_ID, "completed"),
        )

        // Model a process death after the idempotent server ACK returned but before cleanup.
        val recreated = journal(storage)
        assertEquals("Backup complete", recreated.acknowledgementCompletionDetail(JOB_ID))
        storage.failNextCommit = true
        assertThrows(DurableTransferPersistenceException::class.java) {
            recreated.confirmAcknowledged(JOB_ID, transferRecord(JOB_ID, "completed"))
        }
        assertEquals("Backup complete", journal(storage).acknowledgementCompletionDetail(JOB_ID))

        journal(storage).confirmAcknowledged(JOB_ID, transferRecord(JOB_ID, "completed"))
        assertTrue(journal(storage).pendingAcknowledgementJobIds().isEmpty())
    }

    @Test
    fun moreThanEightInterruptedArchiveJobsRetainEveryAcknowledgement() {
        val storage = FakeDurableStorage()
        repeat(24) { index ->
            val jobId = "backup-interrupted-$index"
            queuedJournal(storage, jobId).consumeTerminalResult(
                jobId,
                "Backup $index complete",
                transferRecord(jobId, "completed"),
            )
        }

        val recreated = journal(storage)
        assertEquals(24, recreated.pendingAcknowledgementJobIds().size)
        repeat(24) { index ->
            val jobId = "backup-interrupted-$index"
            assertEquals("Backup $index complete", recreated.acknowledgementCompletionDetail(jobId))
        }
    }

    @Test
    fun mismatchedPendingJobMetadataFailsClosedBeforeRequest() {
        val storage = FakeDurableStorage()
        storage.disk["pending_$JOB_ID"] = JSONObject()
            .put("schemaVersion", 1)
            .put("jobId", JOB_ID)
            .put("path", BACKUP_PATH)
            .put("payload", backupPayload("backup-other"))
            .put("mode", BACKUP_MODE)
            .put("treeUri", SOURCE_URI)
            .toString()

        assertThrows(IllegalArgumentException::class.java) {
            journal(storage).preparePending(JOB_ID)
        }
        assertEquals(0, storage.successfulCommits)
    }

    @Test
    fun mismatchedAcknowledgementJobMetadataFailsClosedBeforeAck() {
        val storage = FakeDurableStorage()
        storage.disk["acknowledgement_$JOB_ID"] = JSONObject()
            .put("schemaVersion", 1)
            .put("jobId", "backup-other")
            .put("requestDigest", "a".repeat(64))
            .put("completionDetail", "Complete")
            .toString()
        storage.disk["transfer_$JOB_ID"] = transferRecord(JOB_ID, "completed").toString()

        var acknowledgementCalls = 0
        runCatching {
            journal(storage).acknowledgementCompletionDetail(JOB_ID)
            acknowledgementCalls += 1
        }
        assertEquals(0, acknowledgementCalls)
    }

    @Test
    fun acknowledgementBoundToDifferentTerminalRecordFailsClosedBeforeAck() {
        val storage = FakeDurableStorage()
        queuedJournal(storage, JOB_ID).consumeTerminalResult(
            JOB_ID,
            "Backup complete",
            transferRecord(JOB_ID, "completed"),
        )
        storage.disk["transfer_$JOB_ID"] = transferRecord(JOB_ID, "completed")
            .put("detail", "Different terminal result")
            .toString()

        var acknowledgementCalls = 0
        runCatching {
            journal(storage).acknowledgementCompletionDetail(JOB_ID)
            acknowledgementCalls += 1
        }
        assertEquals(0, acknowledgementCalls)
    }

    @Test
    fun restorePlanAndJobMustAllMatchBeforeRequest() {
        val storage = FakeDurableStorage()
        val payload = JSONObject()
            .put("restoreRequest", JSONObject().put("planId", "plan-a"))
            .put("expectedPlanId", "plan-a")
            .put("expectedPlanDigest", "digest-a")
            .put(
                "planReference",
                JSONObject()
                    .put("jobId", JOB_ID)
                    .put("planId", "plan-a")
                    .put("planDigest", "digest-b"),
            )
        assertThrows(IllegalArgumentException::class.java) {
            journal(storage).saveQueued(
                JOB_ID,
                "/api/v1/restores/archive/execute",
                payload,
                "saf_restore",
                "content://restore",
                transferRecord(JOB_ID, "queued"),
            )
        }
        assertTrue(storage.disk.isEmpty())
    }

    private fun queuedJournal(storage: FakeDurableStorage, jobId: String): DurableTransferJournal =
        journal(storage).also {
            it.saveQueued(
                jobId,
                BACKUP_PATH,
                backupPayload(jobId),
                BACKUP_MODE,
                SOURCE_URI,
                transferRecord(jobId, "queued"),
            )
        }

    private fun journal(storage: FakeDurableStorage) = DurableTransferJournal(
        storage,
        object : TransferValueProtector {
            override fun protect(value: String): String = value
            override fun unprotect(value: String): String = value
        },
    )

    private fun backupPayload(jobId: String) = JSONObject()
        .put("jobId", jobId)
        .put("displayName", "Documents")

    private fun transferRecord(jobId: String, state: String) = JSONObject()
        .put("jobId", jobId)
        .put("state", state)

    private class FakeDurableStorage : DurableTransferStorage {
        val disk = linkedMapOf<String, String>()
        var failNextCommit = false
        var successfulCommits = 0

        override fun read(key: String): String? = disk[key]
        override fun keys(): Set<String> = disk.keys.toSet()

        override fun commit(puts: Map<String, String>, removals: Set<String>): Boolean {
            if (failNextCommit) {
                failNextCommit = false
                return false
            }
            val replacement = LinkedHashMap(disk)
            removals.forEach(replacement::remove)
            replacement.putAll(puts)
            disk.clear()
            disk.putAll(replacement)
            successfulCommits += 1
            return true
        }
    }

    private companion object {
        const val JOB_ID = "backup-durable-1"
        const val BACKUP_PATH = "/api/v1/backups/archive"
        const val BACKUP_MODE = "saf_backup"
        const val SOURCE_URI = "content://documents/source"
    }
}
