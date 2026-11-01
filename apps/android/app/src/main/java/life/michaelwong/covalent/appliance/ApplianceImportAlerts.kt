package life.michaelwong.covalent.appliance

import java.io.Closeable
import java.security.MessageDigest
import java.util.concurrent.ScheduledFuture
import java.util.concurrent.ScheduledThreadPoolExecutor
import java.util.concurrent.TimeUnit

/** Informational only: the notification holds no import command or approval capability. */
internal data class ApplianceImportAlert(
    val selected: SelectedAppliance, val key: String, val sources: List<PiImportControlSource>,
    val files: Long, val bytes: Long, val available: Long,
) { override fun toString() = "ApplianceImportAlert(<redacted>)" }

internal fun importAlert(state: PiImportsState): ApplianceImportAlert? {
    val selected = state.selected ?: return null
    val observation = state.observation ?: return null
    val context = observation.context ?: return null
    if (!state.actionsAvailable || !observation.supported || observation.generation != selected.confirmedGeneration ||
        context.bond.controller != selected.local || context.bond.appliance != selected.appliance ||
        context.bond.nativeGeneration != selected.confirmedGeneration || !context.volumesAvailable ||
        context.spaceAfterReserve == null || state.actions.any { it.choice.bond == context.bond && !it.resolved }) return null
    val sources = context.sources.filter { source ->
        !source.source.camera && !source.source.automatic && source.source.connected &&
            source.source.preview == PiPreviewState.READY && source.confirmation != null &&
            (source.source.count ?: 0) > 0 && source.source.bytes != null && source.source.bytes <= context.spaceAfterReserve &&
            state.actions.none { it.choice.bond == context.bond && it.outcome in setOf(PiImportOutcome.ACCEPTED, PiImportOutcome.WITHDRAWN) &&
                it.choice.sourceId == source.source.id && (it.choice.confirmation ?: it.source.confirmation) == source.confirmation }
    }.sortedBy { it.source.id }
    if (sources.isEmpty()) return null
    val key = listOf(selected.local.deviceId, selected.local.publicKey, selected.appliance.publicKey, selected.appliance.deviceId,
        context.bond.rootGeneration, context.bond.nativeGeneration).joinToString("|") + sources.joinToString("|") {
        "|${it.source.id}|${it.confirmation!!.attachmentId}|${it.confirmation.previewId}"
    }
    val digest = MessageDigest.getInstance("SHA-256").digest(key.toByteArray()).joinToString("") { "%02x".format(it.toInt() and 255) }
    return ApplianceImportAlert(selected, digest, sources,
        sources.fold(0L) { total, source -> Math.addExact(total, source.source.count!!) },
        sources.fold(0L) { total, source -> Math.addExact(total, source.source.bytes!!) }, context.spaceAfterReserve)
}

internal interface ApplianceImportAlertSink {
    fun enabled(): Boolean
    fun showing(): Boolean
    fun post(alert: ApplianceImportAlert, silent: Boolean): Boolean
    fun cancel()
}

/** One process-scoped I/O worker, active only while its exact native service owner runs. */
internal class ApplianceImportAlertLoop(
    private val read: () -> ApplianceImportAlert?,
    private val selected: () -> SelectedAppliance?,
    private val sink: ApplianceImportAlertSink,
    private val intervalMillis: Long = 30_000,
) : Closeable {
    private val worker = ScheduledThreadPoolExecutor(1) { task -> Thread(task, "interlink-drive-alerts") }
        .apply { removeOnCancelPolicy = true }
    private var owner: Any? = null
    private var epoch = 0L
    private var task: ScheduledFuture<*>? = null
    private var delivered: String? = null
    private var restore = false

    @Synchronized fun start(owner: Any) {
        if (this.owner === owner) return
        task?.cancel(false)
        this.owner = owner
        val token = ++epoch
        task = worker.scheduleWithFixedDelay({ poll(token) }, 0, intervalMillis, TimeUnit.MILLISECONDS)
    }

    @Synchronized fun stop(owner: Any) {
        if (this.owner !== owner) return
        this.owner = null;epoch++
        task?.cancel(false);task = null
        hide()
    }

    private fun poll(token: Long) {
        try {
            synchronized(this) {
                if (owner == null || token != epoch) return
                if (!sink.enabled()) { hide();return }
            }
            val alert = runCatching(read).getOrNull()
            synchronized(this) {
                if (owner == null || token != epoch) return
                if (alert == null || selected() != alert.selected || !sink.enabled()) { hide();return }
                // A dismissed alert stays dismissed for this preview during this process lifetime.
                if (delivered != alert.key || restore || sink.showing()) {
                    if (sink.post(alert, silent = delivered == alert.key)) { delivered = alert.key;restore = false }
                }
            }
        } catch (_: Exception) {
            // Notification failures cannot terminate the scheduled loop or native sync owner.
            synchronized(this) { if (owner != null && token == epoch) runCatching { hide() } }
        }
    }

    private fun hide() { restore = restore || sink.showing();sink.cancel() }

    @Synchronized override fun close() {
        owner = null;epoch++;task?.cancel(false);task = null
        try { sink.cancel() } finally { worker.shutdownNow() }
    }
}
