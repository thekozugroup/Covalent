package life.michaelwong.covalent

import android.app.Application
import androidx.work.Configuration
import life.michaelwong.covalent.node.EmbeddedNodeManager

/** Reserves low JobScheduler IDs for WorkManager; Covalent transfer jobs start at 10,000. */
class CovalentApplication : Application(), Configuration.Provider {
    override fun onCreate() {
        super.onCreate()
        EmbeddedNodeManager(this).reconnectIfEnabled()
    }

    override val workManagerConfiguration: Configuration = Configuration.Builder()
        .setJobSchedulerJobIdRange(0, 9_999)
        .build()
}
