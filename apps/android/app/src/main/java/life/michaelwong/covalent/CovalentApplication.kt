package life.michaelwong.covalent

import android.app.Application
import androidx.work.Configuration

/** Reserves low JobScheduler IDs for WorkManager; Covalent transfer jobs start at 10,000. */
class CovalentApplication : Application(), Configuration.Provider {
    override val workManagerConfiguration: Configuration = Configuration.Builder()
        .setJobSchedulerJobIdRange(0, 9_999)
        .build()
}
