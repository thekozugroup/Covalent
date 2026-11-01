package life.michaelwong.covalent

import android.Manifest
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.text.format.Formatter
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import java.text.NumberFormat
import life.michaelwong.covalent.appliance.ApplianceImportAlert
import life.michaelwong.covalent.appliance.ApplianceImportAlertSink

/** Opens fresh review only. No drive identity, credential or approval is put in an Intent. */
internal class InterlinkImportNotifications(private val context: Context) : ApplianceImportAlertSink {
    private val manager = context.getSystemService(NotificationManager::class.java)
    init {
        runCatching { manager.createNotificationChannel(NotificationChannel(CHANNEL, context.getString(R.string.interlink_alert_channel),
            NotificationManager.IMPORTANCE_DEFAULT).apply {
            description = context.getString(R.string.interlink_alert_channel_detail)
            lockscreenVisibility = Notification.VISIBILITY_PRIVATE
        }) }
    }
    override fun enabled(): Boolean = runCatching {
        (Build.VERSION.SDK_INT < 33 || context.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED) &&
            NotificationManagerCompat.from(context).areNotificationsEnabled() &&
            manager.getNotificationChannel(CHANNEL)?.importance?.let { it > NotificationManager.IMPORTANCE_NONE } == true
    }.getOrDefault(false)

    override fun showing() = manager.activeNotifications.any { it.id == ID && it.tag == TAG }
    override fun cancel() { manager.cancel(TAG, ID) }
    override fun post(alert: ApplianceImportAlert, silent: Boolean): Boolean {
        if (Build.VERSION.SDK_INT >= 33 && context.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) return false
        if (!enabled()) return false
        manager.notify(TAG, ID, notification(alert, silent))
        return true
    }
    internal fun notification(alert: ApplianceImportAlert, silent: Boolean = false): Notification {
        val launch = checkNotNull(context.packageManager.getLaunchIntentForPackage(context.packageName))
        check(launch.component?.packageName == context.packageName)
        launch.addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP)
        val pending = PendingIntent.getActivity(context, ID, launch, PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE)
        val title = context.resources.getQuantityString(R.plurals.interlink_alert_title, alert.sources.size, alert.sources.size)
        val counts = context.getString(R.string.interlink_alert_counts, NumberFormat.getIntegerInstance().format(alert.files),
            Formatter.formatShortFileSize(context, alert.bytes))
        val detail = context.getString(R.string.interlink_alert_body, alert.selected.name,
            alert.sources.joinToString(", ") { it.source.name }, counts,
            Formatter.formatShortFileSize(context, alert.available))
        val public = NotificationCompat.Builder(context, CHANNEL).setSmallIcon(R.drawable.ic_sync_notification)
            .setContentTitle(context.getString(R.string.interlink_alert_private)).build()
        return NotificationCompat.Builder(context, CHANNEL).setSmallIcon(R.drawable.ic_sync_notification)
            .setContentTitle(title).setContentText(counts).setStyle(NotificationCompat.BigTextStyle().bigText(detail))
            .setContentIntent(pending).setOnlyAlertOnce(true).setAutoCancel(true).setSilent(silent)
            .setCategory(NotificationCompat.CATEGORY_REMINDER).setVisibility(NotificationCompat.VISIBILITY_PRIVATE)
            .setPublicVersion(public).build()
    }
    companion object {
        const val CHANNEL = "interlink_drive_review"
        const val ID = 3108
        const val TAG = "interlink_drive_review"
    }
}
