package life.michaelwong.covalent.work

import android.content.Context
import androidx.core.net.toUri
import java.io.InterruptedIOException
import life.michaelwong.covalent.R
import life.michaelwong.covalent.data.CovalentNodeClient
import life.michaelwong.covalent.data.ArchiveTransferResult
import life.michaelwong.covalent.data.NodeApiException
import life.michaelwong.covalent.data.SafTransferBridge
import life.michaelwong.covalent.data.SecureNodeStore
import life.michaelwong.covalent.model.TransferState

internal enum class TransferOutcome {
    SUCCESS,
    RETRY,
    FAILURE,
}

/** Runs one encrypted, persisted transfer request without coupling it to an Android scheduler. */
internal object TransferExecution {
    fun run(context: Context, jobId: String): TransferOutcome {
        val store = SecureNodeStore(context)
        val pending = store.pending(jobId) ?: return TransferOutcome.FAILURE
        when (store.transfer(jobId)?.state) {
            TransferState.PAUSED, TransferState.CANCELLED, TransferState.COMPLETED -> {
                return TransferOutcome.FAILURE
            }
            else -> Unit
        }
        store.updateTransfer(jobId) {
            it.copy(
                state = TransferState.RUNNING,
                detail = context.getString(R.string.transfer_running_detail),
                retryable = false,
            )
        }
        val progress = ProgressRecorder(store, jobId)
        return try {
            val path = pending.getString("path")
            val payload = pending.getJSONObject("payload")
            val mode = pending.optString("mode", "json")
            val client = CovalentNodeClient(store::enrolledTrust)
            val completed = when (mode) {
                TransferWorker.MODE_SAF_BACKUP -> SafTransferBridge(client).createBackup(
                    context,
                    store.baseUrl,
                    store.token,
                    pending.getString("treeUri").toUri(),
                    payload,
                    progress::record,
                )
                TransferWorker.MODE_SAF_RESTORE -> SafTransferBridge(client).restore(
                    context,
                    store.baseUrl,
                    store.token,
                    pending.getString("treeUri").toUri(),
                    payload,
                    progress::record,
                )
                else -> ArchiveTransferResult(
                    body = client.post(store.baseUrl, store.token, path, payload),
                    acknowledgementRequired = false,
                )
            }
            if (mode == TransferWorker.MODE_SAF_BACKUP || path == "/api/v1/backups") {
                store.replaceBackups(client.backups(store.baseUrl, store.token))
            }
            val completion = completionDetail(context, mode, completed.body)
            if (completed.acknowledgementRequired) {
                store.savePendingAcknowledgement(jobId, completion)
            }
            progress.flush()
            store.updateTransfer(jobId) {
                it.copy(
                    state = TransferState.COMPLETED,
                    detail = if (completed.acknowledgementRequired) {
                        context.getString(R.string.transfer_cleanup_pending_detail, completion)
                    } else {
                        completion
                    },
                    retryable = false,
                )
            }
            store.removePending(jobId)
            if (completed.acknowledgementRequired) {
                runCatching { client.acknowledgeJob(store.baseUrl, store.token, jobId) }.onSuccess {
                    store.removePendingAcknowledgement(jobId)
                    store.updateTransfer(jobId) { it.copy(detail = completion) }
                }
            }
            TransferOutcome.SUCCESS
        } catch (error: NodeApiException) {
            failUnlessStopped(store, jobId, error.message, error.retryable)
            if (error.retryable) TransferOutcome.RETRY else TransferOutcome.FAILURE
        } catch (error: InterruptedIOException) {
            val state = store.transfer(jobId)?.state
            if (state != TransferState.PAUSED && state != TransferState.CANCELLED) {
                store.updateTransfer(jobId) {
                    it.copy(
                        state = TransferState.QUEUED,
                        detail = context.getString(R.string.transfer_interrupted_detail),
                        retryable = true,
                    )
                }
            }
            TransferOutcome.RETRY
        } catch (error: SecurityException) {
            failUnlessStopped(store, jobId, error.message, false)
            TransferOutcome.FAILURE
        } catch (error: IllegalArgumentException) {
            failUnlessStopped(store, jobId, error.message, false)
            TransferOutcome.FAILURE
        } catch (error: IllegalStateException) {
            failUnlessStopped(store, jobId, error.message, false)
            TransferOutcome.FAILURE
        } catch (error: Exception) {
            failUnlessStopped(store, jobId, error.message, true)
            TransferOutcome.RETRY
        }
    }

    private fun failUnlessStopped(
        store: SecureNodeStore,
        jobId: String,
        message: String?,
        retryable: Boolean,
    ) {
        store.updateTransfer(jobId) { current ->
            if (current.state == TransferState.PAUSED || current.state == TransferState.CANCELLED) {
                current
            } else {
                current.copy(
                    state = if (retryable) TransferState.QUEUED else TransferState.FAILED,
                    detail = message.orEmpty(),
                    retryable = retryable,
                )
            }
        }
    }

    private fun completionDetail(context: Context, mode: String, result: org.json.JSONObject): String =
        when (mode) {
            TransferWorker.MODE_SAF_BACKUP -> context.getString(
                R.string.transfer_backup_complete_detail,
                result.optLong("entries", 0),
                result.optLong("bytesRead", 0),
                result.optInt("selectedProviders", 0),
                result.optInt("degradedFailures", 0),
            )
            TransferWorker.MODE_SAF_RESTORE -> context.getString(
                R.string.transfer_restore_complete_detail,
                result.optLong("filesRestored", 0),
                result.optLong("bytesWritten", 0),
            )
            else -> context.getString(R.string.transfer_complete_detail)
        }

    fun reconcileAcknowledgements(store: SecureNodeStore, client: CovalentNodeClient): Int {
        var reconciled = 0
        store.pendingAcknowledgementJobIds().forEach { jobId ->
            runCatching { client.acknowledgeJob(store.baseUrl, store.token, jobId) }.onSuccess {
                val completion = store.acknowledgementCompletionDetail(jobId)
                store.removePendingAcknowledgement(jobId)
                store.removePending(jobId)
                if (completion != null) {
                    store.updateTransfer(jobId) { it.copy(detail = completion) }
                }
                reconciled += 1
            }
        }
        store.pendingDiscardJobIds().forEach { jobId ->
            runCatching { client.discardJob(store.baseUrl, store.token, jobId) }.onSuccess {
                val completion = store.discardCompletionDetail(jobId)
                store.removePendingDiscard(jobId)
                store.removePending(jobId)
                if (completion != null) {
                    store.updateTransfer(jobId) { it.copy(detail = completion) }
                }
                reconciled += 1
            }
        }
        return reconciled
    }

    private class ProgressRecorder(
        private val store: SecureNodeStore,
        private val jobId: String,
    ) {
        private var lastBytes = 0L
        private var lastEntries = 0L
        private var lastSavedAt = 0L

        fun record(bytes: Long, entries: Long) {
            lastBytes = bytes
            lastEntries = entries
            val now = System.currentTimeMillis()
            if (now - lastSavedAt >= PROGRESS_PERSIST_INTERVAL_MILLIS) {
                persist()
                lastSavedAt = now
            }
        }

        fun flush() = persist()

        private fun persist() {
            store.updateTransfer(jobId) {
                it.copy(completedBytes = lastBytes, completedEntries = lastEntries)
            }
        }
    }

    private const val PROGRESS_PERSIST_INTERVAL_MILLIS = 500L
}
