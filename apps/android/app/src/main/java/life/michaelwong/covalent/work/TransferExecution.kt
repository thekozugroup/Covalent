package life.michaelwong.covalent.work

import android.content.Context
import androidx.core.net.toUri
import life.michaelwong.covalent.data.CovalentNodeClient
import life.michaelwong.covalent.data.NodeApiException
import life.michaelwong.covalent.data.SafTransferBridge
import life.michaelwong.covalent.data.SecureNodeStore

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
        return try {
            val path = pending.getString("path")
            val payload = pending.getJSONObject("payload")
            val mode = pending.optString("mode", "json")
            val client = CovalentNodeClient()
            when (mode) {
                TransferWorker.MODE_SAF_BACKUP -> SafTransferBridge(client).createBackup(
                    context,
                    store.baseUrl,
                    store.token,
                    pending.getString("treeUri").toUri(),
                    payload,
                )
                TransferWorker.MODE_SAF_RESTORE -> SafTransferBridge(client).restore(
                    context,
                    store.baseUrl,
                    store.token,
                    pending.getString("treeUri").toUri(),
                    payload.getJSONObject("plan"),
                )
                else -> client.post(store.baseUrl, store.token, path, payload)
            }
            if (mode == TransferWorker.MODE_SAF_BACKUP || path == "/api/v1/backups") {
                store.replaceBackups(client.backups(store.baseUrl, store.token))
            }
            store.removePending(jobId)
            TransferOutcome.SUCCESS
        } catch (error: NodeApiException) {
            if (error.retryable) {
                TransferOutcome.RETRY
            } else {
                store.removePending(jobId)
                TransferOutcome.FAILURE
            }
        } catch (_: SecurityException) {
            store.removePending(jobId)
            TransferOutcome.FAILURE
        } catch (_: IllegalArgumentException) {
            store.removePending(jobId)
            TransferOutcome.FAILURE
        } catch (_: IllegalStateException) {
            store.removePending(jobId)
            TransferOutcome.FAILURE
        } catch (_: Exception) {
            TransferOutcome.RETRY
        }
    }
}
