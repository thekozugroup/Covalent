package life.michaelwong.covalent.node

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import androidx.core.app.NotificationCompat
import life.michaelwong.covalent.R

/** Installed-app resources supply the brand; tapping always opens that same app. */
internal object NodeProviderNotification {
    private const val CHANNEL_ID = "covalent_node_provider"
    private const val FLAGS = PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE

    fun runningStatus(context: Context, backupEnabled: Boolean, accessUnavailable: Boolean): String =
        context.getString(when {
            accessUnavailable -> R.string.node_provider_status_access_unavailable
            backupEnabled -> R.string.node_provider_status_backup_available
            else -> R.string.node_provider_status_folders_available
        })

    fun createChannel(context: Context) {
        context.getSystemService(NotificationManager::class.java).createNotificationChannel(
            NotificationChannel(CHANNEL_ID, context.getString(R.string.node_provider_channel_name),
                NotificationManager.IMPORTANCE_LOW).apply {
                description = context.getString(R.string.node_provider_channel_description)
                setShowBadge(false)
            },
        )
    }

    fun create(context: Context, status: String): Notification {
        val stop = Intent(NodeProviderService.ACTION_STOP).setClass(context, NodeProviderService::class.java)
        val launch = context.packageManager.getLaunchIntentForPackage(context.packageName)
            ?.takeIf { it.component?.packageName == context.packageName }
        return NotificationCompat.Builder(context, CHANNEL_ID)
            .setSmallIcon(R.drawable.ic_sync_notification)
            .setContentTitle(context.getString(R.string.node_provider_notification_title))
            .setContentText(status)
            .setContentIntent(launch?.let { PendingIntent.getActivity(context, 0, it, FLAGS) })
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .addAction(0, context.getString(R.string.node_provider_stop), PendingIntent.getService(context, 0, stop, FLAGS))
            .build()
    }
}
