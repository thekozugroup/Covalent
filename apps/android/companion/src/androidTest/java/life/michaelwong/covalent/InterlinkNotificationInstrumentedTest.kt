package life.michaelwong.covalent

import android.app.Notification
import android.app.NotificationManager
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Color
import androidx.core.content.ContextCompat
import androidx.test.platform.app.InstrumentationRegistry
import androidx.test.filters.SdkSuppress
import life.michaelwong.covalent.node.NodeProviderNotification
import life.michaelwong.covalent.work.TransferNotification
import org.junit.Assert.*
import org.junit.Test

/** Exercises the compiled brand overlay and production builders without starting the node. */
@SdkSuppress(minSdkVersion = 31)
class InterlinkNotificationInstrumentedTest {
    private val context = InstrumentationRegistry.getInstrumentation().targetContext

    @Test fun installedBrandAndImmutableActionsBelongToInterlink() {
        assertEquals("Interlink", context.getString(R.string.app_name))
        NodeProviderNotification.createChannel(context)
        val status = NodeProviderNotification.runningStatus(context, false, false)
        val notification = NodeProviderNotification.create(context, status)
        assertEquals("Interlink sync service", notification.extras.getString(Notification.EXTRA_TITLE))
        assertEquals("Ready to sync shared folders", notification.extras.getString(Notification.EXTRA_TEXT))
        assertEquals(R.drawable.ic_sync_notification, notification.smallIcon.resId)
        assertEquals(context.packageName, notification.smallIcon.resPackage)
        assertEquals(context.packageName, notification.contentIntent.creatorPackage)
        assertTrue(notification.contentIntent.isImmutable)
        assertEquals(context.packageName, notification.actions.single().actionIntent.creatorPackage)
        assertTrue(notification.actions.single().actionIntent.isImmutable)
        assertTrue(notification.flags and Notification.FLAG_ONGOING_EVENT != 0)
        assertTrue(notification.flags and Notification.FLAG_ONLY_ALERT_ONCE != 0)
        val channel = context.getSystemService(NotificationManager::class.java).getNotificationChannel(notification.channelId)
        assertEquals("Interlink sync service", channel.name.toString())
        assertEquals(NotificationManager.IMPORTANCE_LOW, channel.importance)
        assertFalse(channel.canShowBadge())
        val transfer = TransferNotification.create(context)
        assertEquals("Interlink transfer in progress", transfer.extras.getString(Notification.EXTRA_TITLE))
        assertEquals(R.drawable.ic_sync_notification, transfer.smallIcon.resId)
    }

    @Test fun folderAttentionTakesPriorityOverReadinessAndNeverClaimsCloudBackup() {
        assertEquals("Shared folders need attention", NodeProviderNotification.runningStatus(context, false, true))
        assertEquals("Shared folders need attention", NodeProviderNotification.runningStatus(context, true, true))
        assertEquals("Ready to receive device backups", NodeProviderNotification.runningStatus(context, true, false))
        assertEquals("Ready to sync shared folders", NodeProviderNotification.runningStatus(context, false, false))
    }

    @Test fun smallIconHasTransparentCornersAndOneOpaqueColour() {
        val bitmap = Bitmap.createBitmap(96, 96, Bitmap.Config.ARGB_8888)
        try {
            val icon = checkNotNull(ContextCompat.getDrawable(context, R.drawable.ic_sync_notification))
            icon.setBounds(0, 0, 96, 96)
            icon.draw(Canvas(bitmap))
            assertEquals(0, Color.alpha(bitmap.getPixel(0, 0)))
            assertEquals(0, Color.alpha(bitmap.getPixel(95, 95)))
            val pixels = IntArray(96 * 96)
            bitmap.getPixels(pixels, 0, 96, 0, 0, 96, 96)
            assertTrue(pixels.any { Color.alpha(it) == 255 })
            assertTrue(pixels.filter { Color.alpha(it) != 0 }.all {
                Color.red(it) == 255 && Color.green(it) == 255 && Color.blue(it) == 255
            })
        } finally { bitmap.recycle() }
    }
}
