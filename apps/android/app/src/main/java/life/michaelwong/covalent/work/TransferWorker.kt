package life.michaelwong.covalent.work

import android.app.NotificationChannel
import android.app.NotificationManager
import android.content.Context
import android.os.Build
import androidx.core.app.NotificationCompat
import androidx.work.CoroutineWorker
import androidx.work.ForegroundInfo
import androidx.work.WorkerParameters
import life.michaelwong.covalent.R
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.runInterruptible

/** Legacy scheduler for Android 13 and earlier. Newer releases use a user-initiated transfer job. */
class TransferWorker(appContext: Context, parameters: WorkerParameters) : CoroutineWorker(appContext, parameters) {
    override suspend fun doWork(): Result {
        val jobId = inputData.getString(KEY_JOB_ID) ?: return Result.failure()
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
            // A migrated WorkManager record may still be present after an app update. The
            // user-initiated JobService owns this request on API 34+, so never start an FGS here.
            return Result.failure()
        }
        setForeground(ForegroundInfo(TRANSFER_NOTIFICATION_ID, TransferNotification.create(applicationContext)))
        return when (runInterruptible(Dispatchers.IO) { TransferExecution.run(applicationContext, jobId) }) {
            TransferOutcome.SUCCESS -> Result.success()
            TransferOutcome.RETRY -> Result.retry()
            TransferOutcome.FAILURE -> Result.failure()
        }
    }

    companion object {
        const val KEY_JOB_ID = "job_id"
        const val MODE_SAF_BACKUP = "saf_backup"
        const val MODE_SAF_RESTORE = "saf_restore"
        const val TRANSFER_NOTIFICATION_ID = 7001
    }
}

internal object TransferNotification {
    private const val CHANNEL_ID = "covalent_transfers"

    fun create(context: Context): android.app.Notification {
        val manager = context.getSystemService(NotificationManager::class.java)
        manager.createNotificationChannel(NotificationChannel(
            CHANNEL_ID,
            context.getString(R.string.transfer_channel_name),
            NotificationManager.IMPORTANCE_LOW,
        ).apply { description = context.getString(R.string.transfer_channel_description) })
        return NotificationCompat.Builder(context, CHANNEL_ID)
            .setSmallIcon(R.drawable.ic_launcher)
            .setContentTitle(context.getString(R.string.transfer_notification_title))
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .build()
    }
}
