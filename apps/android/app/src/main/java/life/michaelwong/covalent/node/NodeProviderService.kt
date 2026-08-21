package life.michaelwong.covalent.node

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.Service
import android.content.Intent
import android.content.pm.PackageManager
import android.content.pm.ServiceInfo
import android.os.IBinder
import android.os.Build
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import life.michaelwong.covalent.R

/** Explicit foreground owner for an opted-in Android storage provider. */
class NodeProviderService : Service() {
    private lateinit var manager: EmbeddedNodeManager
    private var handle: Long = 0L

    override fun onCreate() {
        super.onCreate()
        manager = EmbeddedNodeManager(applicationContext)
        createChannel()
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int = when (intent?.action) {
        ACTION_STOP -> {
            stopProvider()
            START_NOT_STICKY
        }
        else -> {
            startAsConnectedDeviceForeground("Android may pause this provider when it needs resources.")
            val response = manager.serviceStart()
            handle = response.handle ?: 0L
            manager.report(response)
            if (response.ok && handle > 0L) {
                if (Build.VERSION.SDK_INT < 33 || checkSelfPermission(android.Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED) {
                    NotificationManagerCompat.from(this).notify(NOTIFICATION_ID, notification("Android provider is available"))
                }
                START_STICKY
            } else {
                stopForeground(STOP_FOREGROUND_REMOVE)
                stopSelf(startId)
                START_NOT_STICKY
            }
        }
    }

    override fun onDestroy() {
        stopProvider()
        super.onDestroy()
    }

    override fun onBind(intent: Intent?): IBinder? = null

    private fun stopProvider() {
        val response = manager.serviceStop(handle)
        handle = 0L
        manager.report(response)
        stopForeground(STOP_FOREGROUND_REMOVE)
        stopSelf()
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
        const val ACTION_STOP = "life.michaelwong.covalent.node.STOP"
        private const val CHANNEL_ID = "covalent_node_provider"
        private const val NOTIFICATION_ID = 3107
    }
}
