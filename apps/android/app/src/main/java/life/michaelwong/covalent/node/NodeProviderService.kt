package life.michaelwong.covalent.node

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.Service
import android.content.Intent
import android.content.pm.PackageManager
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.Handler
import android.os.HandlerThread
import android.os.IBinder
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import life.michaelwong.covalent.R

/** Pure serialized stop/restart state; a failed stop can never clear [reaping]. */
internal class NodeServiceReapState {
    var reaping: Boolean = false
        private set
    private var pendingStart = false
    private var destroying = false

    fun requestStart() {
        if (!destroying) pendingStart = true
    }

    fun requestStop(restartAfterExit: Boolean) {
        reaping = true
        pendingStart = restartAfterExit && !destroying
    }

    fun requestDestroy() {
        destroying = true
        pendingStart = false
        reaping = true
    }

    fun stopFailed() {
        check(reaping)
    }

    fun confirmedExitShouldRestart(): Boolean {
        check(reaping)
        reaping = false
        return pendingStart && !destroying
    }
}

/** Prevents two Android Service instances in this process from owning the same node root. */
internal object NodeServiceProcessOwner {
    private var owner: Any? = null

    @Synchronized
    fun tryAcquire(token: Any): Boolean {
        if (owner == null || owner === token) owner = token
        return owner === token
    }

    @Synchronized
    fun release(token: Any) {
        if (owner === token) owner = null
    }
}

/** Explicit foreground owner for an opted-in Android storage provider or folder-sync host. */
class NodeProviderService : Service() {
    // Constructing EmbeddedNodeManager reads private storage and computes provider usage. The lazy
    // is first touched only by actor work, never by onCreate/onStartCommand on the main looper.
    private val manager by lazy(LazyThreadSafetyMode.NONE) { EmbeddedNodeManager(applicationContext) }
    private lateinit var actorThread: HandlerThread
    private lateinit var actor: Handler

    // Every field below is confined to actorThread. In particular, a stop failure retains `handle`
    // and no queued command can launch a replacement before that exact handle confirms exit.
    private var handle: Long = 0L
    private var launchedAccessUnavailable = false
    private var launchedDemand = NodeServiceDemand(backupEnabled = false, folderSyncRequested = false)
    private var latestStartId = 0
    private val reapState = NodeServiceReapState()
    private val ownershipToken = Any()
    private var ownsProcess = false
    private var ownershipRetryScheduled = false
    private var waitingForOwnership: PendingCommand? = null
    private var pendingAfterReap: PendingCommand? = null
    @Volatile
    private var destroyRequested = false

    private data class PendingCommand(val action: String, val startId: Int)

    private val ownershipRetry = object : Runnable {
        override fun run() {
            ownershipRetryScheduled = false
            val command = waitingForOwnership ?: return
            if (destroyRequested) {
                waitingForOwnership = null
                actorThread.quitSafely()
            } else if (acquireProcessOwnership()) {
                waitingForOwnership = null
                handleOwnedCommand(command)
            } else {
                scheduleOwnershipRetry()
            }
        }
    }

    private val accessCheck = object : Runnable {
        override fun run() {
            if (handle <= 0L) return
            if (reapState.reaping) {
                stopProvider()
            } else if (
                nodeServiceConfigurationChanged(
                    launchedAccessUnavailable,
                    launchedDemand,
                    manager.folderSyncAccessUnavailable(),
                    manager.nodeServiceDemand(),
                )
            ) {
                beginRestartForAccessChange()
            } else {
                actor.postDelayed(this, ACCESS_CHECK_MILLIS)
            }
        }
    }

    override fun onCreate() {
        super.onCreate()
        actorThread = HandlerThread("covalent-node-owner").apply { start() }
        actor = Handler(actorThread.looper)
        createChannel()
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val action = intent?.action ?: ACTION_START
        if (action != ACTION_STOP) {
            startAsConnectedDeviceForeground(
                if (action == ACTION_RECOVER) "Recovering this phone's Covalent identity"
                else "Covalent's on-phone node is starting",
            )
        }
        actor.post { handleCommand(action, startId) }
        return if (action == ACTION_STOP) START_NOT_STICKY else START_STICKY
    }

    override fun onDestroy() {
        // The actor retains this service until the exact native handle exits. Android may kill the
        // process, but ordinary destruction/cancellation cannot drop ownership and launch another
        // helper in this process while the previous one is still reaping.
        destroyRequested = true
        actor.post {
            waitingForOwnership = null
            pendingAfterReap = null
            actor.removeCallbacks(ownershipRetry)
            actor.removeCallbacks(accessCheck)
            if (!ownsProcess) {
                stopForeground(STOP_FOREGROUND_REMOVE)
                actorThread.quitSafely()
            } else if (handle > 0L) {
                reapState.requestDestroy()
                stopProvider()
            } else {
                stopForeground(STOP_FOREGROUND_REMOVE)
                releaseProcessOwnership()
                actorThread.quitSafely()
            }
        }
        RecoveryBootstrapHandoff.finish()
        super.onDestroy()
    }

    override fun onBind(intent: Intent?): IBinder? = null

    private fun handleCommand(action: String, startId: Int) {
        latestStartId = startId
        val command = PendingCommand(action, startId)
        if (destroyRequested) return
        if (!acquireProcessOwnership()) {
            waitingForOwnership = command
            scheduleOwnershipRetry()
            return
        }
        handleOwnedCommand(command)
    }

    private fun handleOwnedCommand(command: PendingCommand) {
        latestStartId = command.startId
        if (destroyRequested) {
            reapState.requestDestroy()
            if (handle > 0L) stopProvider()
            return
        }
        if (reapState.reaping) {
            if (command.action == ACTION_STOP) {
                pendingAfterReap = null
                reapState.requestStop(restartAfterExit = false)
            } else {
                pendingAfterReap = command
                reapState.requestStart()
            }
            stopProvider()
            return
        }
        when (command.action) {
            ACTION_STOP -> {
                RecoveryBootstrapHandoff.cancel()
                pendingAfterReap = null
                reapState.requestStop(restartAfterExit = false)
                stopProvider()
            }
            ACTION_RECOVER -> recover(command.startId)
            ACTION_REFRESH_ACCESS -> startOrRefresh(command.startId, "Checking folder access")
            else -> startOrRefresh(command.startId, "Covalent's on-phone node is running")
        }
    }

    private fun recover(startId: Int) {
        val request = RecoveryBootstrapHandoff.take()
        if (handle > 0L) {
            request?.close()
            RecoveryBootstrapHandoff.finish()
            manager.report(
                NativeNodeResponse(
                    ok = true,
                    code = "node_already_running",
                    message = "Stop the on-phone node before recovering an identity.",
                    handle = null,
                    apiBaseUrl = null,
                    peerAddress = null,
                    state = "running",
                ),
            )
            scheduleAccessCheck()
            return
        }
        val response = try {
            request?.let(manager::serviceRecover) ?: NativeNodeResponse(
                ok = false,
                code = "recovery_request_unavailable",
                message = "The recovery selection expired. Choose both files again.",
                handle = null,
                apiBaseUrl = null,
                peerAddress = null,
                state = "stopped",
            )
        } finally {
            request?.close()
            RecoveryBootstrapHandoff.finish()
        }
        finishStart(
            response,
            startId,
            manager.folderSyncAccessUnavailable(),
            manager.nodeServiceDemand(),
        )
    }

    private fun startOrRefresh(startId: Int, unchangedStatus: String) {
        if (
            handle > 0L && !nodeServiceConfigurationChanged(
                launchedAccessUnavailable,
                launchedDemand,
                manager.folderSyncAccessUnavailable(),
                manager.nodeServiceDemand(),
            )
        ) {
            scheduleAccessCheck()
            updateNotification(unchangedStatus)
        } else if (handle > 0L) {
            beginRestartForAccessChange()
        } else {
            startNode(startId)
        }
    }

    private fun stopProvider() {
        check(reapState.reaping)
        actor.removeCallbacks(accessCheck)
        val response = manager.serviceStop(handle)
        if (!response.ok) {
            reapState.stopFailed()
            manager.reportStopFailure(response)
            actor.postDelayed(accessCheck, ACCESS_CHECK_MILLIS)
            return
        }
        handle = 0L
        manager.report(response)
        if (destroyRequested) reapState.requestDestroy()
        val shouldRestart = reapState.confirmedExitShouldRestart()
        if (destroyRequested) {
            pendingAfterReap = null
            stopForeground(STOP_FOREGROUND_REMOVE)
            releaseProcessOwnership()
            actorThread.quitSafely()
        } else if (shouldRestart) {
            val command = pendingAfterReap ?: PendingCommand(ACTION_START, latestStartId)
            pendingAfterReap = null
            startAfterReap(command)
        } else {
            pendingAfterReap = null
            stopForeground(STOP_FOREGROUND_REMOVE)
            releaseProcessOwnership()
            stopSelf()
        }
    }

    private fun startNode(startId: Int) {
        val accessUnavailable = manager.folderSyncAccessUnavailable()
        val demand = manager.nodeServiceDemand()
        finishStart(manager.serviceStart(), startId, accessUnavailable, demand)
    }

    private fun finishStart(
        response: NativeNodeResponse,
        startId: Int,
        accessUnavailableAtLaunch: Boolean,
        demandAtLaunch: NodeServiceDemand,
    ) {
        handle = response.handle ?: 0L
        launchedAccessUnavailable = accessUnavailableAtLaunch
        launchedDemand = demandAtLaunch
        manager.report(response)
        if (destroyRequested && handle > 0L) {
            reapState.requestDestroy()
            stopProvider()
            return
        }
        if (response.ok && handle > 0L) {
            scheduleAccessCheck()
            updateNotification(
                if (demandAtLaunch.backupEnabled) "Android provider is available"
                else "Shared folders are available",
            )
        } else {
            stopForeground(STOP_FOREGROUND_REMOVE)
            releaseProcessOwnership()
            stopSelf(startId)
        }
    }

    /** Never launches a replacement until native stop confirms that the exact handle exited. */
    private fun beginRestartForAccessChange() {
        val restart = manager.serviceNeeded()
        pendingAfterReap = if (restart) PendingCommand(ACTION_REFRESH_ACCESS, latestStartId) else null
        reapState.requestStop(restartAfterExit = restart)
        stopProvider()
    }

    private fun startAfterReap(command: PendingCommand) {
        if (destroyRequested || !manager.serviceNeeded()) {
            stopForeground(STOP_FOREGROUND_REMOVE)
            releaseProcessOwnership()
            stopSelf()
            return
        }
        when (command.action) {
            ACTION_RECOVER -> recover(command.startId)
            else -> startNode(command.startId)
        }
    }

    private fun acquireProcessOwnership(): Boolean {
        if (!ownsProcess) ownsProcess = NodeServiceProcessOwner.tryAcquire(ownershipToken)
        return ownsProcess
    }

    private fun releaseProcessOwnership() {
        if (ownsProcess) NodeServiceProcessOwner.release(ownershipToken)
        ownsProcess = false
    }

    private fun scheduleOwnershipRetry() {
        if (!ownershipRetryScheduled) {
            ownershipRetryScheduled = true
            actor.postDelayed(ownershipRetry, ACCESS_CHECK_MILLIS)
        }
    }

    private fun scheduleAccessCheck() {
        actor.removeCallbacks(accessCheck)
        actor.postDelayed(accessCheck, ACCESS_CHECK_MILLIS)
    }

    private fun updateNotification(status: String) {
        if (
            Build.VERSION.SDK_INT < 33 ||
            checkSelfPermission(android.Manifest.permission.POST_NOTIFICATIONS) ==
            PackageManager.PERMISSION_GRANTED
        ) {
            NotificationManagerCompat.from(this).notify(NOTIFICATION_ID, notification(status))
        }
    }

    private fun createChannel() {
        getSystemService(NotificationManager::class.java).createNotificationChannel(
            NotificationChannel(
                CHANNEL_ID,
                getString(R.string.node_provider_channel_name),
                NotificationManager.IMPORTANCE_LOW,
            ).apply {
                description = getString(R.string.node_provider_channel_description)
                setShowBadge(false)
            },
        )
    }

    private fun startAsConnectedDeviceForeground(status: String) {
        if (Build.VERSION.SDK_INT >= 29) {
            startForeground(
                NOTIFICATION_ID,
                notification(status),
                ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE,
            )
        } else {
            startForeground(NOTIFICATION_ID, notification(status))
        }
    }

    private fun notification(status: String): Notification {
        // setClass binds the destination component explicitly, so the stop action can never
        // resolve to another package; FLAG_IMMUTABLE additionally stops any holder of the
        // PendingIntent from retargeting or populating it.
        val stopIntent = Intent(ACTION_STOP).setClass(this, NodeProviderService::class.java)
        val stopPendingIntent = android.app.PendingIntent.getService(
            this,
            0,
            stopIntent,
            android.app.PendingIntent.FLAG_UPDATE_CURRENT or android.app.PendingIntent.FLAG_IMMUTABLE,
        )
        return NotificationCompat.Builder(this, CHANNEL_ID)
            .setSmallIcon(R.drawable.ic_launcher)
            .setContentTitle(getString(R.string.node_provider_notification_title))
            .setContentText(status)
            .setOngoing(true)
            .addAction(0, getString(R.string.node_provider_stop), stopPendingIntent)
            .build()
    }

    companion object {
        const val ACTION_START = "life.michaelwong.covalent.node.START"
        const val ACTION_RECOVER = "life.michaelwong.covalent.node.RECOVER"
        const val ACTION_STOP = "life.michaelwong.covalent.node.STOP"
        const val ACTION_REFRESH_ACCESS = "life.michaelwong.covalent.node.REFRESH_SYNC_ACCESS"
        private const val CHANNEL_ID = "covalent_node_provider"
        private const val NOTIFICATION_ID = 3107
        private const val ACCESS_CHECK_MILLIS = 3_000L
    }
}
