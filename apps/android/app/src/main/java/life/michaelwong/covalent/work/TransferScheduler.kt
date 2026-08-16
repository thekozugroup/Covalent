package life.michaelwong.covalent.work

import android.app.job.JobInfo
import android.app.job.JobParameters
import android.app.job.JobScheduler
import android.app.job.JobService
import android.content.ComponentName
import android.content.Context
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.os.PersistableBundle
import androidx.annotation.RequiresApi
import androidx.work.Constraints
import androidx.work.ExistingWorkPolicy
import androidx.work.NetworkType
import androidx.work.OneTimeWorkRequestBuilder
import androidx.work.WorkManager
import androidx.work.workDataOf
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.Future
import java.util.concurrent.FutureTask
import java.util.concurrent.RejectedExecutionException
import java.util.concurrent.ThreadPoolExecutor
import java.util.concurrent.TimeUnit
import life.michaelwong.covalent.data.SecureNodeStore
import life.michaelwong.covalent.model.TransferState
import life.michaelwong.covalent.R

internal enum class TransferExecutionModel {
    LEGACY_WORK_MANAGER,
    USER_INITIATED_JOB,
}

internal fun transferExecutionModel(sdkInt: Int): TransferExecutionModel =
    if (sdkInt >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
        TransferExecutionModel.USER_INITIATED_JOB
    } else {
        TransferExecutionModel.LEGACY_WORK_MANAGER
    }

/** Selects the platform transfer primitive and keeps encrypted pending jobs recoverable. */
internal object TransferScheduler {
    fun enqueue(context: Context, jobId: String) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
            scheduleUserInitiated(context, jobId)
        } else {
            scheduleLegacy(context, jobId)
        }
    }

    /**
     * Android force-stop cancels scheduled work. A foreground relaunch requeues only requests
     * whose encrypted pending records still exist; normal process death needs no intervention.
     */
    fun requeuePending(context: Context, store: SecureNodeStore) {
        store.runnablePendingJobIds().forEach { jobId ->
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
                WorkManager.getInstance(context).cancelUniqueWork(workName(jobId))
            }
            enqueue(context, jobId)
        }
    }

    fun cancelScheduled(context: Context, jobId: String) {
        WorkManager.getInstance(context).cancelUniqueWork(workName(jobId))
        val scheduler = context.getSystemService(JobScheduler::class.java)
        scheduler.allPendingJobs
            .filter { it.extras.getString(TransferWorker.KEY_JOB_ID) == jobId }
            .forEach { scheduler.cancel(it.id) }
    }

    @RequiresApi(Build.VERSION_CODES.UPSIDE_DOWN_CAKE)
    internal fun buildUserInitiatedJob(context: Context, jobId: String, schedulerId: Int): JobInfo =
        JobInfo.Builder(schedulerId, ComponentName(context, TransferJobService::class.java))
            .setExtras(PersistableBundle().apply { putString(TransferWorker.KEY_JOB_ID, jobId) })
            .setRequiredNetworkType(JobInfo.NETWORK_TYPE_ANY)
            .setBackoffCriteria(RETRY_BACKOFF_MILLIS, JobInfo.BACKOFF_POLICY_EXPONENTIAL)
            .setPersisted(true)
            .setUserInitiated(true)
            .build()

    internal fun stableSchedulerId(jobId: String): Int =
        JOB_ID_BASE + (jobId.hashCode() and JOB_ID_HASH_MASK)

    @RequiresApi(Build.VERSION_CODES.UPSIDE_DOWN_CAKE)
    private fun scheduleUserInitiated(context: Context, jobId: String) {
        val scheduler = context.getSystemService(JobScheduler::class.java)
        check(scheduler.canRunUserInitiatedJobs()) {
            "Android has disabled user-initiated transfers for Covalent. Re-enable the app and try again."
        }
        val schedulerId = resolveSchedulerId(scheduler, jobId)
        val existing = scheduler.getPendingJob(schedulerId)
        if (existing?.extras?.getString(TransferWorker.KEY_JOB_ID) == jobId) return
        check(scheduler.schedule(buildUserInitiatedJob(context, jobId, schedulerId)) == JobScheduler.RESULT_SUCCESS) {
            "Android could not schedule the requested transfer. Keep Covalent open and try again."
        }
    }

    @RequiresApi(Build.VERSION_CODES.UPSIDE_DOWN_CAKE)
    private fun resolveSchedulerId(scheduler: JobScheduler, jobId: String): Int {
        var candidate = stableSchedulerId(jobId)
        repeat(MAX_JOB_ID_PROBES) {
            val existing = scheduler.getPendingJob(candidate)
            if (existing == null || existing.extras.getString(TransferWorker.KEY_JOB_ID) == jobId) {
                return candidate
            }
            candidate += 1
        }
        error("Android has no available transfer job identifier.")
    }

    private fun scheduleLegacy(context: Context, jobId: String) {
        val request = OneTimeWorkRequestBuilder<TransferWorker>()
            .setConstraints(Constraints.Builder().setRequiredNetworkType(NetworkType.CONNECTED).build())
            .setInputData(workDataOf(TransferWorker.KEY_JOB_ID to jobId))
            .addTag(workName(jobId))
            .build()
        WorkManager.getInstance(context).enqueueUniqueWork(workName(jobId), ExistingWorkPolicy.KEEP, request)
    }

    private fun workName(jobId: String) = "covalent-$jobId"

    private const val JOB_ID_BASE = 10_000
    private const val JOB_ID_HASH_MASK = 0x3fff_ffff
    private const val MAX_JOB_ID_PROBES = 1_024
    private const val RETRY_BACKOFF_MILLIS = 30_000L
}

/**
 * Android 14+ long-running, user-requested network transfer service. JobScheduler owns the
 * lifecycle and notification, so background execution never starts a forbidden foreground service.
 */
class TransferJobService : JobService() {
    private val executor = ThreadPoolExecutor(
        1,
        MAX_CONCURRENT_TRANSFERS,
        30,
        TimeUnit.SECONDS,
        ArrayBlockingQueue(MAX_QUEUED_TRANSFERS),
        ThreadPoolExecutor.AbortPolicy(),
    ).apply { allowCoreThreadTimeOut(true) }
    private val running = ConcurrentHashMap<JobParameters, Future<*>>()
    private val mainHandler = Handler(Looper.getMainLooper())

    override fun onStartJob(params: JobParameters): Boolean {
        val jobId = params.extras.getString(TransferWorker.KEY_JOB_ID)?.takeIf { it.isNotBlank() }
            ?: return false
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE && params.isUserInitiatedJob) {
            setNotification(
                params,
                TransferWorker.TRANSFER_NOTIFICATION_ID + (params.jobId and NOTIFICATION_ID_MASK),
                TransferNotification.create(this),
                JOB_END_NOTIFICATION_POLICY_REMOVE,
            )
        }
        val task = FutureTask<Unit> {
            val outcome = TransferExecution.run(applicationContext, jobId)
            if (running.remove(params) != null) {
                mainHandler.post { jobFinished(params, outcome == TransferOutcome.RETRY) }
            }
        }
        running[params] = task
        try {
            executor.execute(task)
        } catch (_: RejectedExecutionException) {
            running.remove(params)
            SecureNodeStore(applicationContext).updateTransfer(jobId) {
                it.copy(
                    state = TransferState.QUEUED,
                    detail = getString(R.string.transfer_waiting_capacity_detail),
                    retryable = true,
                )
            }
            mainHandler.post { jobFinished(params, true) }
        }
        return true
    }

    override fun onStopJob(params: JobParameters): Boolean {
        running.remove(params)?.cancel(true)
        val jobId = params.extras.getString(TransferWorker.KEY_JOB_ID) ?: return false
        val store = SecureNodeStore(applicationContext)
        val record = store.transfer(jobId)
        if (record?.state == TransferState.RUNNING) {
            store.updateTransfer(jobId) {
                it.copy(
                    state = TransferState.QUEUED,
                    detail = getString(R.string.transfer_system_paused_detail),
                    retryable = true,
                )
            }
        }
        return store.pending(jobId) != null && record?.state !in setOf(
            TransferState.PAUSED,
            TransferState.CANCELLED,
            TransferState.FAILED,
            TransferState.COMPLETED,
        )
    }

    override fun onNetworkChanged(params: JobParameters) {
        // Transfers use device loopback to reach the local daemon. The connectivity constraint is
        // required by user-initiated jobs, but switching the external network changes no endpoint.
    }

    override fun onDestroy() {
        running.values.forEach { it.cancel(true) }
        running.clear()
        executor.shutdownNow()
        super.onDestroy()
    }

    private companion object {
        const val NOTIFICATION_ID_MASK = 0x0fff
        const val MAX_CONCURRENT_TRANSFERS = 2
        const val MAX_QUEUED_TRANSFERS = 16
    }
}
