package life.michaelwong.covalent.node

/** Optional Application observer. Callbacks must enqueue work, never perform network I/O. */
interface NodeHostLifecycle {
    fun nodeHostStarted(owner: Any)
    fun nodeHostStopped(owner: Any)
}
