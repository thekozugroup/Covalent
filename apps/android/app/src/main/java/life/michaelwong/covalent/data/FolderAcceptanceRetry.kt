package life.michaelwong.covalent.data

/** Retry only the accept endpoint's explicit pre-mutation lock refusal. */
internal fun <T> retryFolderAcceptance(
    wait: (Long) -> Unit = Thread::sleep,
    accept: () -> T,
): T {
    return retryExplicitFolderBusy(wait, accept)
}

/** Reuse the exact settings change ID/revision/body only on the explicit busy response. */
internal fun <T> retryFolderLinkSettings(
    wait: (Long) -> Unit = Thread::sleep,
    change: () -> T,
): T = retryExplicitFolderBusy(wait, change)

private fun <T> retryExplicitFolderBusy(wait: (Long) -> Unit, request: () -> T): T {
    val delays = longArrayOf(100, 200, 400, 800, 1_600)
    var retries = 0
    while (true) {
        if (Thread.currentThread().isInterrupted) throw InterruptedException("Folder acceptance cancelled.")
        try {
            return request()
        } catch (busy: NodeApiException) {
            // Other responses and lost/invalid responses may have committed.
            // Never replay those, or a different request/change ID.
            if (busy.statusCode != 503 || busy.protocolVersion != 1 || busy.code != "folder_sync_busy" ||
                !busy.retryable || !busy.hasExplicitRetryContract || retries == delays.size
            ) throw busy
            try {
                wait(delays[retries++])
            } catch (cancelled: InterruptedException) {
                Thread.currentThread().interrupt()
                throw cancelled
            }
        }
    }
}
