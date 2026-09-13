package life.michaelwong.covalent.node

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.os.BatteryManager
import android.os.Build
import android.os.Handler
import android.os.SystemClock
import life.michaelwong.covalent.model.FolderSharePhase
import life.michaelwong.covalent.sync.FolderSyncApi

internal data class AndroidConditionSnapshot(val wifiConnected: Boolean, val charging: Boolean)

/** Pure timing policy. Reports immediately on changes and every minute while shared conditions are used. */
internal class AndroidConditionHeartbeat {
    private var active = false
    private var conditionsUsed = false
    private var lastSnapshot: AndroidConditionSnapshot? = null
    private var lastReportAt = Long.MIN_VALUE
    private var lastRefreshAt = Long.MIN_VALUE

    fun start() { active = true }
    fun stop() {
        active = false
        conditionsUsed = false
        lastSnapshot = null
    }
    fun shouldRefresh(nowMs: Long): Boolean = active && elapsed(nowMs, lastRefreshAt) >= REFRESH_MILLIS
    fun refreshed(used: Boolean, nowMs: Long) {
        conditionsUsed = used
        lastRefreshAt = nowMs
    }
    fun shouldReport(snapshot: AndroidConditionSnapshot, nowMs: Long): Boolean = active &&
        (snapshot != lastSnapshot || conditionsUsed && elapsed(nowMs, lastReportAt) >= REPORT_MILLIS)
    fun reported(snapshot: AndroidConditionSnapshot, nowMs: Long) {
        lastSnapshot = snapshot
        lastReportAt = nowMs
    }

    private fun elapsed(now: Long, then: Long): Long = if (then == Long.MIN_VALUE) Long.MAX_VALUE else now - then

    private companion object {
        const val REFRESH_MILLIS = 60_000L
        const val REPORT_MILLIS = 60_000L
    }
}

/** Process-local observer owned by the existing managed node service. */
internal class AndroidConditionFeed(
    private val context: Context,
    private val actor: Handler,
    private val api: FolderSyncApi,
    private val connection: () -> life.michaelwong.covalent.model.NodeConnection?,
) {
    private val heartbeat = AndroidConditionHeartbeat()
    private val connectivity = context.getSystemService(ConnectivityManager::class.java)
    private var charging = false
    private var registered = false
    private var networkRegistered = false
    private var batteryRegistered = false

    private val networkCallback = object : ConnectivityManager.NetworkCallback() {
        override fun onAvailable(network: Network) = changed()
        override fun onLost(network: Network) = changed()
        override fun onCapabilitiesChanged(network: Network, capabilities: NetworkCapabilities) = changed()
    }
    private val batteryReceiver = object : BroadcastReceiver() {
        override fun onReceive(context: Context?, intent: Intent?) {
            charging = intent?.isCharging() ?: charging
            changed()
        }
    }

    fun start() {
        if (registered) return
        heartbeat.start()
        networkRegistered = runCatching {
            connectivity.registerDefaultNetworkCallback(networkCallback)
        }.isSuccess
        val filter = IntentFilter(Intent.ACTION_BATTERY_CHANGED)
        @Suppress("DEPRECATION")
        val sticky = runCatching {
            if (Build.VERSION.SDK_INT >= 33) {
                context.registerReceiver(batteryReceiver, filter, Context.RECEIVER_NOT_EXPORTED)
            } else context.registerReceiver(batteryReceiver, filter)
        }.onSuccess { batteryRegistered = true }.getOrNull()
        charging = sticky?.isCharging() ?: false
        registered = true
        report(SystemClock.elapsedRealtime(), refresh = true)
    }

    fun heartbeat() {
        if (registered) report(SystemClock.elapsedRealtime(), refresh = false)
    }

    fun stop() {
        heartbeat.stop()
        if (!registered) return
        if (networkRegistered) runCatching { connectivity.unregisterNetworkCallback(networkCallback) }
        if (batteryRegistered) runCatching { context.unregisterReceiver(batteryReceiver) }
        networkRegistered = false
        batteryRegistered = false
        registered = false
    }

    private fun changed() {
        if (registered) actor.post { report(SystemClock.elapsedRealtime(), refresh = false) }
    }

    private fun report(nowMs: Long, refresh: Boolean) {
        val ready = connection() ?: return
        if (refresh || heartbeat.shouldRefresh(nowMs)) {
            val used = runCatching { api.status(ready) }.getOrNull()?.shares?.any { share ->
                share.phase != FolderSharePhase.REMOVED && share.linkSettings?.confirmed == true &&
                    share.linkSettings.settings.androidConditions.let { it.wifiOnly || it.chargingOnly }
            } ?: return
            heartbeat.refreshed(used, nowMs)
        }
        val snapshot = AndroidConditionSnapshot(
            wifiConnected = connectivity.getNetworkCapabilities(connectivity.activeNetwork)
                ?.hasTransport(NetworkCapabilities.TRANSPORT_WIFI) == true,
            charging = charging,
        )
        if (heartbeat.shouldReport(snapshot, nowMs)) {
            runCatching { api.conditions(ready, snapshot.wifiConnected, snapshot.charging) }
                .onSuccess { heartbeat.reported(snapshot, nowMs) }
        }
    }

    private fun Intent.isCharging(): Boolean = getIntExtra(BatteryManager.EXTRA_STATUS, -1).let {
        it == BatteryManager.BATTERY_STATUS_CHARGING || it == BatteryManager.BATTERY_STATUS_FULL
    }
}
