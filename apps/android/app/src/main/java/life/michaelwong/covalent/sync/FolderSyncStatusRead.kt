package life.michaelwong.covalent.sync

import java.io.IOException
import kotlinx.coroutines.delay
import life.michaelwong.covalent.data.NodeApiException
import life.michaelwong.covalent.model.FolderSyncStatus

/** A permission change restarts the local API. Retry reads only; never replay a mutation. */
internal suspend fun readFolderSyncStatusWhenReady(
    attempts: Int = 32,
    pause: suspend () -> Unit = { delay(250) },
    read: () -> FolderSyncStatus,
): FolderSyncStatus {
    require(attempts > 0)
    repeat(attempts) { attempt ->
        try {
            return read()
        } catch (error: Exception) {
            val transient = error is IOException || error is NodeApiException && error.retryable
            if (!transient || attempt == attempts - 1) throw error
        }
        pause()
    }
    error("Unreachable status retry")
}
