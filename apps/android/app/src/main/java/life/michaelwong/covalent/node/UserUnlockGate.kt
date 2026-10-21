package life.michaelwong.covalent.node

/** The observer returns an unregister action after registration is complete. */
internal fun runAfterUserUnlock(
    isUnlocked: () -> Boolean,
    observeUnlock: (() -> Unit) -> (() -> Unit),
    action: () -> Unit,
) {
    if (isUnlocked()) {
        action()
        return
    }
    val lock = Any()
    var removeObserver: (() -> Unit)? = null
    var delivered = false
    fun attempt() {
        val remove = synchronized(lock) {
            // A callback may arrive before registration returns its cleanup handle.
            // The post-registration check below covers that case, too.
            if (delivered || removeObserver == null || !isUnlocked()) return
            delivered = true
            checkNotNull(removeObserver).also { removeObserver = null }
        }
        remove()
        action()
    }
    val remove = observeUnlock(::attempt)
    synchronized(lock) { removeObserver = remove }
    // USER_UNLOCKED is not sticky. It may have arrived between the initial
    // check and registration, so an observer alone can wait forever.
    attempt()
}
