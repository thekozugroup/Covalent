package life.michaelwong.covalent.node

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.net.NetworkRequest
import android.os.BatteryManager
import android.os.Build
import android.os.Handler
import android.os.SystemClock
import life.michaelwong.covalent.model.FolderSharePhase
import life.michaelwong.covalent.sync.FolderSyncApi

internal data class AndroidConditionSnapshot(val wifiConnected: Boolean, val charging: Boolean)

/** Actor-confined transport state. Tokens reject callbacks queued by an earlier service lifetime. */
internal class AndroidConditionTransportState<T> {
    private var epoch = 0L
    private var active = false
    private val wifiNetworks = mutableSetOf<T>()
    private var charging = false

    fun start(initialWifi: Collection<T>, initialCharging: Boolean): Long {
        epoch += 1
        active = true
        wifiNetworks.clear()
        wifiNetworks.addAll(initialWifi)
        charging = initialCharging
        return epoch
    }

    fun stop() {
        epoch += 1
        active = false
        wifiNetworks.clear()
    }

    fun updateWifi(token: Long, network: T, available: Boolean): AndroidConditionSnapshot? {
        if (!active || token != epoch) return null
        if (available) wifiNetworks += network else wifiNetworks -= network
        return snapshot()
    }

    fun replaceWifi(token: Long, networks: Collection<T>): AndroidConditionSnapshot? {
        if (!active || token != epoch) return null
        wifiNetworks.clear()
        wifiNetworks.addAll(networks)
        return snapshot()
    }

    fun updateCharging(token: Long, value: Boolean): AndroidConditionSnapshot? {
        if (!active || token != epoch) return null
        charging = value
        return snapshot()
    }

    fun snapshot(token: Long): AndroidConditionSnapshot? =
        if (active && token == epoch) snapshot() else null

    private fun snapshot() = AndroidConditionSnapshot(wifiNetworks.isNotEmpty(), charging)
}

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

/** Process-local observer owned by the existing managed node service. All methods run on [actor]. */
internal class AndroidConditionFeed(
    private val context: Context,
    private val actor: Handler,
    private val api: FolderSyncApi,
    private val connection: () -> life.michaelwong.covalent.model.NodeConnection?,
) {
    private val heartbeat = AndroidConditionHeartbeat()
    private val transport = AndroidConditionTransportState<Network>()
    private val connectivity = context.getSystemService(ConnectivityManager::class.java)
    private var token = 0L
    private var registered = false
    private var networkCallback: ConnectivityManager.NetworkCallback? = null
    private var batteryReceiver: BroadcastReceiver? = null

    fun start() {
        if (registered) return
        heartbeat.start()
        val initialWifi = currentWifiNetworks().orEmpty()
        token = transport.start(initialWifi, initialCharging = false)
        val currentToken = token
        val callback = object : ConnectivityManager.NetworkCallback() {
            override fun onAvailable(network: Network) = wifiChanged(currentToken, network, true)
            override fun onLost(network: Network) = wifiChanged(currentToken, network, false)
            override fun onCapabilitiesChanged(network: Network, capabilities: NetworkCapabilities) =
                wifiChanged(currentToken, network, capabilities.hasTransport(NetworkCapabilities.TRANSPORT_WIFI))
        }
        val request = NetworkRequest.Builder()
            .addTransportType(NetworkCapabilities.TRANSPORT_WIFI)
            .build()
        networkCallback = callback.takeIf {
            runCatching { connectivity.registerNetworkCallback(request, callback, actor) }.isSuccess
        }
        val receiver = object : BroadcastReceiver() {
            override fun onReceive(context: Context?, intent: Intent?) {
                intent?.let { chargingChanged(currentToken, it.isCharging()) }
            }
        }
        val filter = IntentFilter(Intent.ACTION_BATTERY_CHANGED)
        @Suppress("DEPRECATION")
        val sticky = runCatching {
            if (Build.VERSION.SDK_INT >= 33) {
                context.registerReceiver(receiver, filter, null, actor, Context.RECEIVER_NOT_EXPORTED)
            } else context.registerReceiver(receiver, filter, null, actor)
        }
        batteryReceiver = receiver.takeIf { sticky.isSuccess }
        sticky.getOrNull()?.let { transport.updateCharging(currentToken, it.isCharging()) }
        registered = true
        report(currentToken, SystemClock.elapsedRealtime(), refresh = true)
    }

    fun heartbeat() {
        if (registered) report(token, SystemClock.elapsedRealtime(), refresh = false)
    }

    fun stop() {
        heartbeat.stop()
        transport.stop()
        networkCallback?.let { runCatching { connectivity.unregisterNetworkCallback(it) } }
        batteryReceiver?.let { runCatching { context.unregisterReceiver(it) } }
        networkCallback = null
        batteryReceiver = null
        registered = false
    }

    private fun wifiChanged(callbackToken: Long, network: Network, available: Boolean) {
        transport.updateWifi(callbackToken, network, available)?.let {
            report(callbackToken, SystemClock.elapsedRealtime(), refresh = false)
        }
    }

    private fun chargingChanged(callbackToken: Long, charging: Boolean) {
        transport.updateCharging(callbackToken, charging)?.let {
            report(callbackToken, SystemClock.elapsedRealtime(), refresh = false)
        }
    }

    private fun report(callbackToken: Long, nowMs: Long, refresh: Boolean) {
        val ready = connection() ?: return
        if (refresh || heartbeat.shouldRefresh(nowMs)) {
            // Reconcile the callback state without requiring Wi-Fi to be the default network.
            // If Android cannot provide the network list, do not renew a stale observation.
            val wifi = currentWifiNetworks() ?: if (refresh) emptyList() else return
            transport.replaceWifi(callbackToken, wifi) ?: return
            val used = runCatching { api.status(ready) }.getOrNull()?.shares?.any { share ->
                share.phase != FolderSharePhase.REMOVED && share.linkSettings?.confirmed == true &&
                    share.linkSettings.settings.androidConditions.let { it.wifiOnly || it.chargingOnly }
            } ?: return
            heartbeat.refreshed(used, nowMs)
        }
        val snapshot = transport.snapshot(callbackToken) ?: return
        if (heartbeat.shouldReport(snapshot, nowMs)) {
            runCatching { api.conditions(ready, snapshot.wifiConnected, snapshot.charging) }
                .onSuccess { heartbeat.reported(snapshot, nowMs) }
        }
    }

    private fun isWifi(network: Network): Boolean = connectivity.getNetworkCapabilities(network)
        ?.hasTransport(NetworkCapabilities.TRANSPORT_WIFI) == true

    @Suppress("DEPRECATION")
    private fun currentWifiNetworks(): List<Network>? = runCatching {
        connectivity.allNetworks.filter(::isWifi)
    }.getOrNull()

    private fun Intent.isCharging(): Boolean = getIntExtra(BatteryManager.EXTRA_STATUS, -1).let {
        it == BatteryManager.BATTERY_STATUS_CHARGING || it == BatteryManager.BATTERY_STATUS_FULL
    }
}
